use super::client::send_action;
use super::event::AppEvent;
use super::state::{AppState, CreateAction, CreateEntry, CreateStep, InputMode};
use super::view;
use crate::config::Harness;
use crate::daemon::socket::ClientMsg;
use crate::worktree::worktree_name_for_branch;
use crate::zellij;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Lifetime of a footer toast, in spinner ticks (~120ms each); about 3 seconds.
const TOAST_TICKS: u16 = 25;

/// True when `(col, row)` falls inside `r`.
fn point_in(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Map a click in a row region to a 0-based row index, if it lands on a row.
fn row_index(area: Rect, count: usize, col: u16, row: u16) -> Option<usize> {
    if !point_in(area, col, row) {
        return None;
    }
    let idx = (row - area.y) as usize;
    (idx < count).then_some(idx)
}

/// Detect a double-click at the same cell within 400ms.
fn is_double_click(app: &mut AppState, col: u16, row: u16) -> bool {
    let now = Instant::now();
    let dbl = matches!(
        app.last_click,
        Some((c, r, t)) if c == col && r == row && now.duration_since(t) < Duration::from_millis(400)
    );
    // Reset after a double so a third click starts a fresh pair.
    app.last_click = if dbl { None } else { Some((col, row, now)) };
    dbl
}

pub(super) fn spawn_close_tab(tx: mpsc::Sender<AppEvent>, name: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = zellij::close_tab_by_name(&name) {
            let _ = tx.blocking_send(AppEvent::ZellijError(format!("zellij close failed: {e}")));
        }
    });
}

/// Open the worktree's zellij tab if it doesn't exist yet, then switch to it.
///
/// This is the only path that opens worktree tabs: tab pinning is gone, so a
/// worktree gets a tab only when the user activates it (Enter / double-click)
/// or when swamp itself just created the worktree. Querying `query-tab-names`
/// first makes activation idempotent — an existing tab is switched to rather
/// than duplicated. If tab names can't be queried the tab state is unknown, so
/// we do nothing rather than blind-open a possible duplicate. Outside zellij
/// there's no session to act on.
pub(super) fn activate_worktree_tab(tx: mpsc::Sender<AppEvent>, path: PathBuf, name: String) {
    if !zellij::in_zellij() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        let tabs = match zellij::list_tab_names() {
            Ok(tabs) => tabs,
            Err(e) => {
                // Unknown tab state: don't open, just surface the failure.
                tracing::debug!(worktree = %name, "tab query unavailable: {e}");
                return;
            }
        };
        if !tabs.iter().any(|t| t == &name) {
            tracing::info!(worktree = %name, "opening worktree tab on demand");
            if let Err(e) = crate::launch::open_worktree_tab(&path, &name) {
                let _ = tx.blocking_send(AppEvent::ZellijError(format!(
                    "open worktree tab failed: {e}"
                )));
                return;
            }
        }
        if let Err(e) = zellij::go_to_tab_name(&name) {
            let _ = tx.blocking_send(AppEvent::ZellijError(format!("zellij jump failed: {e}")));
        }
    });
}

/// Open (if needed) and switch to the tab for the worktree at `idx`.
fn jump_to_worktree(app: &AppState, tx: mpsc::Sender<AppEvent>, idx: usize) {
    if let Some(r) = app.snapshot.rows.get(idx) {
        activate_worktree_tab(tx, r.path.clone(), r.name.clone());
    }
}

pub(super) fn handle_mouse(
    app: &mut AppState,
    m: MouseEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    // While the create picker is open it owns all mouse input.
    if matches!(app.input, Some(InputMode::Create(_))) {
        handle_create_mouse(app, m, tx, common);
        return;
    }
    if matches!(
        app.input,
        Some(InputMode::ConfirmDelete { .. } | InputMode::PickHarness { .. })
    ) {
        // Swallow all mouse input while a footer prompt is open, but only a real
        // click dismisses it. Mouse capture enables motion reporting (xterm mode
        // 1003), so crossterm emits a `Moved` event for every cursor movement;
        // clearing on those would erase the prompt the instant the cursor twitches.
        if matches!(m.kind, MouseEventKind::Down(_)) {
            app.input = None;
            app.last_click = None;
        }
        return;
    }
    let (col, row) = (m.column, m.row);
    match m.kind {
        // Scroll routes to whatever panel the cursor is over.
        MouseEventKind::ScrollDown => {
            if app.regions.resources.is_some_and(|r| point_in(r, col, row)) {
                let max = view::max_resource_scroll(&app.resources, app.resource_viewport_height);
                app.resource_scroll = (app.resource_scroll + 3).min(max);
            } else if app
                .regions
                .worktrees
                .is_some_and(|(r, _, _)| point_in(r, col, row))
                && !app.snapshot.rows.is_empty()
            {
                app.move_selection(1);
            }
        }
        MouseEventKind::ScrollUp => {
            if app.regions.resources.is_some_and(|r| point_in(r, col, row)) {
                app.resource_scroll = app.resource_scroll.saturating_sub(3);
            } else if app
                .regions
                .worktrees
                .is_some_and(|(r, _, _)| point_in(r, col, row))
            {
                app.move_selection(-1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let dbl = is_double_click(app, col, row);

            // Worktree table: click selects, double-click jumps. Clicking the
            // PR-icon column opens the PR instead.
            if let Some((area, count, offset)) = app.regions.worktrees
                && let Some(row_idx) = row_index(area, count, col, row)
            {
                let idx = offset + row_idx;
                // Fixed leading columns: #(3) + sp + agent(2) + sp = 7,
                // then the 1-wide PR icon.
                let pr_col = area.x + 7;
                if col == pr_col
                    && let Some(url) = app
                        .snapshot
                        .rows
                        .get(idx)
                        .and_then(|r| app.pr_snapshot.prs.get(&r.branch))
                        .and_then(|pr| pr.url.clone())
                {
                    crate::util::open_url(&url);
                    return;
                }
                app.select_index(idx);
                if dbl {
                    jump_to_worktree(app, tx.clone(), idx);
                }
                return;
            }

            // AI status: click selects the matching worktree, double-click jumps.
            let ai_target =
                app.regions.ai.as_ref().and_then(|(area, idxs)| {
                    row_index(*area, idxs.len(), col, row).map(|i| idxs[i])
                });
            if let Some(idx) = ai_target {
                app.select_index(idx);
                if dbl {
                    jump_to_worktree(app, tx.clone(), idx);
                }
                return;
            }

            // PR & CI: click copies the PR URL to the clipboard. OSC 52 reaches
            // the user's own clipboard across SSH, where a local browser opener
            // would not.
            let pr_url = app.regions.prs.as_ref().and_then(|(area, hits)| {
                row_index(*area, hits.len(), col, row).and_then(|i| hits[i].url.clone())
            });
            if let Some(url) = pr_url {
                crate::util::copy_to_clipboard(&url);
                app.toast = Some(("PR URL copied to clipboard".into(), TOAST_TICKS));
            }
        }
        _ => {}
    }
}

/// Spawn a detached `swamp relaunch-tab` to apply a harness swap live. It runs
/// in its own process group so that closing the worktree's tab — which happens
/// when `h` is pressed from that worktree's own sidebar pane — can't kill the
/// process mid-relaunch.
fn spawn_relaunch_tab(name: &str, path: &std::path::Path) {
    use std::os::unix::process::CommandExt;
    if !crate::zellij::in_zellij() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .arg("relaunch-tab")
        .arg(name)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

/// Handle the `d` key on the worktrees pane: open the delete confirmation for
/// the selected row, or reject with a footer message when it's already being
/// deleted. The snapshot already carries the removal verdict computed during
/// the scan, so the confirmation can name a blocking reason immediately —
/// no daemon round-trip needed to learn it, and no request is sent until the
/// user actually confirms in [`handle_input_key`]. `AppEvent::DeleteNeedsForce`
/// remains the fallback for a stale snapshot: it reopens the same prompt with
/// the daemon's own reason when the two disagree.
pub(super) fn handle_delete_key(app: &mut AppState) {
    let Some(row) = app.selected_row() else {
        return;
    };
    if row.deleting {
        app.status_msg = Some(format!("{} is already being deleted", row.name));
        return;
    }
    let name = row.name.clone();
    let force_reason = row
        .removal_block
        .as_ref()
        .map(|reason| reason.description().to_string());
    app.status_msg = None;
    app.input = Some(InputMode::ConfirmDelete {
        name,
        force_reason,
        close_tab: false,
    });
}

/// Handle the `D` key: delete the worktree the pane itself lives in — not
/// whichever row is selected — and close its Zellij tab once the removal
/// completes. A no-op, with a footer message, when the pane's working
/// directory doesn't resolve to a worktree, when that worktree is already
/// being deleted, or when it's the repository default branch's worktree: the
/// dashboard's cwd *is* the default worktree, so an unguarded `D` there would
/// target trunk — never what the user means, and the most destructive
/// possible misfire of a single keystroke. The guard is on the resolved
/// worktree's default-branch flag, not on "is this the dashboard", so a
/// worktree tab genuinely open on the default branch is guarded too; `d` on
/// that same row still runs the normal flow deliberately.
pub(super) fn handle_delete_current_tab_key(app: &mut AppState) {
    let Some(current) = app.current_tab.clone() else {
        return;
    };
    let Some(row) = app.snapshot.rows.iter().find(|r| r.name == current) else {
        return;
    };
    if row.is_default {
        app.status_msg = Some("D does nothing on the default branch worktree".to_string());
        return;
    }
    if row.deleting {
        app.status_msg = Some(format!("{} is already being deleted", row.name));
        return;
    }
    let name = row.name.clone();
    let force_reason = row
        .removal_block
        .as_ref()
        .map(|reason| reason.description().to_string());
    app.status_msg = None;
    app.input = Some(InputMode::ConfirmDelete {
        name,
        force_reason,
        close_tab: true,
    });
}

/// Open a floating `swamp confirm-delete` pane for a blocked deletion: it
/// renders the reason (passed in, never re-computed — the pane never
/// acquires `repo_ops`) plus live status and diffstat, and owns force/cancel
/// from there. `close_tab` is threaded through for the `D` key, which also
/// wants the pane's eventual force-delete to close the originating tab.
///
/// `Ok` means the pane was opened and now owns the rest of the flow; `Err`
/// means the caller should fall back to the footer force-prompt — a spawn
/// failure (an unsupported flag on an older Zellij, or no Zellij at all)
/// must degrade to today's behavior rather than block the delete.
pub(super) fn spawn_confirm_delete_pane(
    common: &std::path::Path,
    name: &str,
    path: &std::path::Path,
    reason: &str,
    close_tab: bool,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "swamp".to_string());
    let path_arg = path.to_string_lossy().into_owned();
    let mut cmd = vec![
        exe.as_str(),
        "confirm-delete",
        name,
        path_arg.as_str(),
        reason,
    ];
    if close_tab {
        cmd.push("--close-tab");
    }
    zellij::new_floating_pane(common, &format!("delete {name}"), &cmd)
}

/// Handle a keystroke while a footer prompt is active. `app.input` was already
/// taken by the caller, so each branch re-stores it to stay open, or leaves it
/// `None` to dismiss the prompt. (The create picker is handled separately by
/// [`handle_create_key`].)
pub(super) fn handle_input_key(
    app: &mut AppState,
    mode: InputMode,
    k: KeyEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    match mode {
        // The create picker keeps its state in `app.input` and is dispatched
        // before this function is reached; it never arrives here.
        InputMode::Create(picker) => {
            app.input = Some(InputMode::Create(picker));
        }
        InputMode::ConfirmDelete {
            name,
            force_reason,
            close_tab,
        } => match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let row_path = app
                    .snapshot
                    .rows
                    .iter()
                    .find(|r| r.name == name)
                    .map(|r| r.path.clone());

                // A blocked deletion inside Zellij opens a floating pane —
                // narrow sidebars can't show the work at risk. The pane owns
                // the rest of the flow from here (force or cancel, and the
                // tab close when `close_tab` is set); nothing else is sent
                // from this pane. A removable row, or a spawn failure, falls
                // back below.
                let opened_pane = force_reason.as_deref().is_some_and(|reason| {
                    zellij::in_zellij()
                        && row_path.as_ref().is_some_and(|path| {
                            spawn_confirm_delete_pane(common, &name, path, reason, close_tab)
                                .is_ok()
                        })
                });
                if opened_pane {
                    return;
                }

                if close_tab {
                    // Every `D` fallback must use the detached helper. It
                    // closes the tab before removal, including when a blocked
                    // deletion could not open its floating pane.
                    if let Some(path) = row_path {
                        crate::launch::spawn_delete_tab(
                            &name,
                            &path,
                            common,
                            force_reason.is_some(),
                        );
                    } else {
                        app.status_msg = Some(format!("Cannot find worktree path for {name}"));
                    }
                    return;
                }

                // Plain removal (`d`) falls back to a direct request.
                // `pending_delete` is
                // kept only to close this pane's tab once the row disappears
                // from a later snapshot (see the Snapshot handler in
                // event.rs) — the "deleting…" status itself now comes from
                // the shared `WorktreeRow::deleting` flag, visible in every
                // subscribed TUI rather than just this pane.
                app.pending_delete = Some(name.clone());
                let tx = tx.clone();
                let common = common.to_path_buf();
                // Use force: true when the daemon already refused once (or
                // the snapshot verdict already predicted it) and we're
                // asking the user to confirm a force override.
                let force = force_reason.is_some();
                tokio::spawn(async move {
                    if let Err(e) = send_action(
                        &common,
                        ClientMsg::RemoveWorktree { name, force },
                        tx.clone(),
                    )
                    .await
                    {
                        let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                    }
                });
            }
            _ => {} // n / Esc / anything else cancels
        },
        InputMode::PickHarness { name } => {
            let harness = match k.code {
                KeyCode::Char('c') | KeyCode::Char('C') => Some(Harness::Claude),
                KeyCode::Char('x') | KeyCode::Char('X') => Some(Harness::Codex),
                _ => None, // Esc / anything else cancels
            };
            if let Some(harness) = harness {
                // The worktree's path, needed to reopen its tab with the new
                // harness once the choice is persisted.
                let path = app
                    .snapshot
                    .rows
                    .iter()
                    .find(|r| r.name == name)
                    .map(|r| r.path.clone());
                app.status_msg = Some(format!("{name} → {}", harness.label()));
                let tx = tx.clone();
                let common = common.to_path_buf();
                let worktree = name.clone();
                tokio::spawn(async move {
                    if let Err(e) = send_action(
                        &common,
                        ClientMsg::SetHarness {
                            worktree: worktree.clone(),
                            harness,
                        },
                        tx.clone(),
                    )
                    .await
                    {
                        let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                        return;
                    }
                    // The daemon has persisted the override by the time it replies
                    // Ok, so reopening the tab now reads the new harness. Run it
                    // detached so closing this worktree's own tab can't abort it.
                    if let Some(path) = path {
                        spawn_relaunch_tab(&worktree, &path);
                    }
                });
            }
        }
    }
}

/// Handle a keystroke while the create picker is open. Mutates the picker in
/// place via `app.input`; Enter is delegated to [`create_confirm`].
pub(super) fn handle_create_key(
    app: &mut AppState,
    k: KeyEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Esc => {
            // From the Base step, Esc steps back to the Branch step (restoring
            // the typed name); from the Branch step it cancels the picker.
            if let Some(InputMode::Create(p)) = app.input.as_mut()
                && p.step == CreateStep::Base
            {
                p.step = CreateStep::Branch;
                p.filter = p.new_branch.take().unwrap_or_default();
                p.selected = 0;
                p.scroll = 0;
                return;
            }
            app.input = None;
        }
        KeyCode::Char('c') if ctrl => app.input = None,
        KeyCode::Enter => create_confirm(app, tx, common),
        KeyCode::Up => create_move_sel(app, -1),
        KeyCode::Down => create_move_sel(app, 1),
        KeyCode::Char('p') if ctrl => create_move_sel(app, -1),
        KeyCode::Char('n') if ctrl => create_move_sel(app, 1),
        KeyCode::Backspace => {
            if let Some(InputMode::Create(p)) = app.input.as_mut() {
                p.filter.pop();
                p.selected = 0;
                p.scroll = 0;
            }
        }
        KeyCode::Char(c) if !ctrl => {
            if let Some(InputMode::Create(p)) = app.input.as_mut() {
                p.filter.push(c);
                p.selected = 0;
                p.scroll = 0;
            }
        }
        _ => {}
    }
}

/// Move the picker selection by `delta`, clamped to the filtered entry list.
fn create_move_sel(app: &mut AppState, delta: i32) {
    if let Some(InputMode::Create(p)) = app.input.as_mut() {
        let n = p.entries().len();
        if n == 0 {
            p.selected = 0;
            return;
        }
        let next = p.selected as i32 + delta;
        p.selected = next.clamp(0, n as i32 - 1) as usize;
    }
}

/// Act on the currently-selected picker entry: advance to the Base step for a
/// new branch, or fire the create request for an existing branch / chosen base.
fn create_confirm(app: &mut AppState, tx: &mpsc::Sender<AppEvent>, common: &std::path::Path) {
    let Some(InputMode::Create(mut picker)) = app.input.take() else {
        return;
    };
    let action = {
        let entries = picker.entries();
        entries.get(picker.selected).map(|e| match e {
            CreateEntry::New(name) => CreateAction::New(name.to_string()),
            CreateEntry::Branch(b) => CreateAction::Branch(b.name.clone()),
        })
    };
    match (picker.step, action) {
        (CreateStep::Branch, Some(CreateAction::New(name))) => {
            picker.step = CreateStep::Base;
            picker.new_branch = Some(name);
            picker.filter.clear();
            picker.selected = picker
                .branches
                .iter()
                .position(|b| b.is_default)
                .unwrap_or(0);
            picker.scroll = 0;
            app.input = Some(InputMode::Create(picker));
        }
        (CreateStep::Branch, Some(CreateAction::Branch(branch))) => {
            start_create(app, tx, common, ClientMsg::CreateWorktree { branch });
        }
        (CreateStep::Base, Some(CreateAction::Branch(base))) => {
            if let Some(branch) = picker.new_branch.clone() {
                start_create(
                    app,
                    tx,
                    common,
                    ClientMsg::CreateWorktreeFromBase { branch, base },
                );
            }
        }
        // Nothing selectable, or an impossible combo: reopen unchanged.
        _ => app.input = Some(InputMode::Create(picker)),
    }
}

/// Fire a worktree-create request and arm the pending-create tracking so only
/// that target's tab opens when the next snapshot arrives. Leaves `app.input`
/// closed.
fn start_create(
    app: &mut AppState,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
    msg: ClientMsg,
) {
    let label = match &msg {
        ClientMsg::CreateWorktree { branch } | ClientMsg::CreateWorktreeFromBase { branch, .. } => {
            branch.clone()
        }
        _ => String::new(),
    };
    app.pending_create = Some(worktree_name_for_branch(&label).to_string());
    app.status_msg = Some(format!("Creating {label}…"));
    let tx = tx.clone();
    let common = common.to_path_buf();
    tokio::spawn(async move {
        if let Err(e) = send_action(&common, msg, tx.clone()).await {
            let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
        }
    });
}

/// Route a mouse event to the open create picker: scroll/click select an entry,
/// double-click confirms it.
fn handle_create_mouse(
    app: &mut AppState,
    m: MouseEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    match m.kind {
        MouseEventKind::ScrollDown => create_move_sel(app, 1),
        MouseEventKind::ScrollUp => create_move_sel(app, -1),
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(area) = app.regions.create_list else {
                return;
            };
            let dbl = is_double_click(app, m.column, m.row);
            if let Some(InputMode::Create(p)) = app.input.as_mut() {
                let n = p.entries().len();
                let visible = n.saturating_sub(p.scroll).min(area.height as usize);
                if let Some(idx) = row_index(area, visible, m.column, m.row) {
                    p.selected = (p.scroll + idx).min(n.saturating_sub(1));
                }
            }
            if dbl {
                create_confirm(app, tx, common);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_in_respects_bounds() {
        let r = Rect {
            x: 2,
            y: 3,
            width: 4,
            height: 2,
        };
        assert!(point_in(r, 2, 3)); // top-left corner
        assert!(point_in(r, 5, 4)); // bottom-right inclusive
        assert!(!point_in(r, 6, 4)); // one past width
        assert!(!point_in(r, 5, 5)); // one past height
        assert!(!point_in(r, 1, 3)); // left of region
    }

    #[test]
    fn row_index_maps_click_to_row() {
        // Rows region with three visible rows starting at y=3.
        let area = Rect {
            x: 0,
            y: 3,
            width: 10,
            height: 5,
        };
        assert_eq!(row_index(area, 3, 0, 3), Some(0));
        assert_eq!(row_index(area, 3, 9, 5), Some(2));
        // Inside the rect but past the populated rows.
        assert_eq!(row_index(area, 3, 0, 6), None);
        // Outside the rect entirely.
        assert_eq!(row_index(area, 3, 0, 2), None);
    }

    use crate::daemon::resources;
    use crate::daemon::state::{AgentStatus, PrSnapshot, Snapshot, WorktreeRow};
    use crate::worktree::RemoveRefusedReason;

    fn row(name: &str, removal_block: Option<RemoveRefusedReason>, deleting: bool) -> WorktreeRow {
        WorktreeRow {
            name: name.to_string(),
            path: PathBuf::from(format!("/repo/{name}")),
            branch: name.to_string(),
            upstream: None,
            upstream_gone: false,
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflict: false,
            rebase: false,
            agent: AgentStatus::Idle,
            agent_ts: 0,
            session_name: None,
            head_ts: 0,
            harness: None,
            is_default: false,
            removal_block,
            deleting,
        }
    }

    fn app_with_row(row: WorktreeRow) -> AppState {
        let name = row.name.clone();
        AppState {
            snapshot: Snapshot { rows: vec![row] },
            selected: Some(name),
            worktree_scroll: 0,
            spinner_frame: 0,
            repo_name: "repo".into(),
            view: crate::cli::TuiView::Worktrees,
            refreshing: false,
            pending_delete: None,
            pending_create: None,
            connected: true,
            input: None,
            status_msg: None,
            toast: None,
            resources: resources::Snapshot::default(),
            pr_snapshot: PrSnapshot::default(),
            resource_scroll: 0,
            resource_viewport_height: 0,
            current_dir: None,
            pin_cwd: false,
            tab_env: None,
            current_tab: None,
            regions: super::super::state::HitRegions::default(),
            last_click: None,
        }
    }

    /// Pressing `d` on a row carrying a blocking reason opens the confirmation
    /// pre-filled with that reason — a force prompt on the very first press,
    /// with no daemon request sent until the user actually confirms.
    #[test]
    fn delete_key_opens_force_prompt_for_blocked_row() {
        let mut app = app_with_row(row("feature", Some(RemoveRefusedReason::Dirty), false));

        handle_delete_key(&mut app);

        match app.input {
            Some(InputMode::ConfirmDelete {
                name,
                force_reason,
                close_tab,
            }) => {
                assert_eq!(name, "feature");
                assert_eq!(
                    force_reason.as_deref(),
                    Some(RemoveRefusedReason::Dirty.description())
                );
                assert!(!close_tab, "the `d` key must never set close_tab");
            }
            _ => panic!("expected a ConfirmDelete prompt"),
        }
    }

    /// A removable row's first prompt has no force reason.
    #[test]
    fn delete_key_opens_plain_prompt_for_removable_row() {
        let mut app = app_with_row(row("feature", None, false));

        handle_delete_key(&mut app);

        match app.input {
            Some(InputMode::ConfirmDelete { force_reason, .. }) => {
                assert_eq!(force_reason, None);
            }
            _ => panic!("expected a ConfirmDelete prompt"),
        }
    }

    /// A row already marked deleting rejects a second `d` with a footer
    /// message instead of opening a prompt.
    #[test]
    fn delete_key_rejects_already_deleting_row() {
        let mut app = app_with_row(row("feature", None, true));

        handle_delete_key(&mut app);

        assert!(app.input.is_none(), "no prompt should open");
        assert!(
            app.status_msg.is_some(),
            "a footer message must explain why"
        );
    }

    /// `D` no-ops on the default-branch worktree — the guard is on the
    /// resolved row's default-branch flag, not on "is this the dashboard" —
    /// while `d` on that same (selected) row still runs the normal
    /// confirmation flow, which is deliberate.
    #[test]
    fn delete_current_tab_key_noops_on_default_branch_but_d_still_works() {
        let mut default_row = row("main", None, false);
        default_row.is_default = true;
        let mut app = app_with_row(default_row);
        app.current_tab = Some("main".to_string());

        handle_delete_current_tab_key(&mut app);
        assert!(
            app.input.is_none(),
            "D must not open a prompt on the default branch"
        );
        assert!(
            app.status_msg.is_some(),
            "a footer message must explain why D did nothing"
        );

        handle_delete_key(&mut app);
        assert!(
            app.input.is_some(),
            "d on the same row must still run the normal confirmation flow"
        );
    }

    /// `D` targets the pane's own worktree (`current_tab`), not whichever row
    /// happens to be selected.
    #[test]
    fn delete_current_tab_key_targets_current_tab_not_selection() {
        let mut app = app_with_row(row("selected-row", None, false));
        app.snapshot.rows.push(row("current-worktree", None, false));
        app.current_tab = Some("current-worktree".to_string());

        handle_delete_current_tab_key(&mut app);

        match app.input {
            Some(InputMode::ConfirmDelete {
                name, close_tab, ..
            }) => {
                assert_eq!(name, "current-worktree");
                assert!(close_tab, "D must set close_tab");
            }
            _ => panic!("expected a ConfirmDelete prompt"),
        }
    }

    /// `D` does nothing when the pane's working directory doesn't resolve to
    /// any known worktree.
    #[test]
    fn delete_current_tab_key_noop_when_unresolved() {
        let mut app = app_with_row(row("feature", None, false));
        app.current_tab = None;

        handle_delete_current_tab_key(&mut app);

        assert!(app.input.is_none());
        assert!(app.status_msg.is_none());
    }
}
