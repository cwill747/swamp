use super::client::send_action;
use super::event::AppEvent;
use super::state::{AppState, CreateAction, CreateEntry, CreateStep, InputMode};
use super::view;
use crate::cli::TuiView;
use crate::config::Harness;
use crate::daemon::socket::ClientMsg;
use crate::worktree::worktree_name_for_branch;
use crate::zellij;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

pub(super) fn spawn_go_to_tab(tx: mpsc::Sender<AppEvent>, name: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = zellij::go_to_tab_name(&name) {
            let _ = tx.blocking_send(AppEvent::ZellijError(format!("zellij jump failed: {e}")));
        }
    });
}

pub(super) fn spawn_close_tab(tx: mpsc::Sender<AppEvent>, name: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = zellij::close_tab_by_name(&name) {
            let _ = tx.blocking_send(AppEvent::ZellijError(format!("zellij close failed: {e}")));
        }
    });
}

pub(super) fn request_reconcile_tabs(tx: mpsc::Sender<AppEvent>, full: bool) {
    if !zellij::in_zellij() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        let tabs = zellij::list_tab_names().map_err(|e| e.to_string());
        let _ = tx.blocking_send(AppEvent::TabsListed { full, tabs });
    });
}

/// Jump the zellij session to the tab for the worktree at `idx`.
fn jump_to_worktree(app: &AppState, tx: mpsc::Sender<AppEvent>, idx: usize) {
    if let Some(r) = app.snapshot.rows.get(idx) {
        spawn_go_to_tab(tx, r.name.clone());
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
        app.input = None;
        app.last_click = None;
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
        InputMode::ConfirmDelete { name, force_reason } => match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                app.pending_delete = Some(name.clone());
                app.status_msg = Some(format!("Deleting {name}…"));
                let tx = tx.clone();
                let common = common.to_path_buf();
                // Use force: true when the daemon already refused once and
                // we're asking the user to confirm a force override.
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

/// Create zellij tabs for worktrees that newly appeared in the snapshot.
///
/// Swamp opens the requested target tab itself when *it* creates a worktree
/// (the `pending_create` path), but a worktree born outside swamp — `git
/// worktree add` in another terminal, an agent spinning one up — only shows up
/// in the daemon snapshot. It lists in the dashboard, yet double-clicking it
/// can't focus anything because no tab exists. Reconcile fills that gap.
///
/// Eligibility is *appearance*, not absence: a tab is opened for a worktree
/// only when its name wasn't in the previously reconciled snapshot (or on the
/// first reconcile after startup, when everything is new to us). A long-known
/// worktree with no tab means the user closed it — treating "no tab" itself as
/// the trigger meant every snapshot broadcast (one per agent hook ping)
/// resurrected tabs the user had just closed. `full` restores the
/// open-anything-missing behavior for the manual refresh key, the deliberate
/// "put my session back together" gesture.
///
/// Only the dashboard's worktrees pane runs this: it's the single instance with
/// `view == Worktrees && !pin_cwd`, so the several swamp panes (one per worktree
/// tab, plus the dashboard's other views) don't race to create duplicate tabs.
/// `query-tab-names` is the dedupe — a worktree that already has a tab is
/// skipped, which also makes the first post-launch snapshot a no-op.
///
/// Bail unless we're inside a zellij session: `query-tab-names` has no session
/// to query when `swamp tui` is run bare in a terminal. A failed tab query must
/// be treated as "unknown", not "empty", because an empty tab set reads as
/// "every worktree is missing a tab" and would spawn a duplicate `new-tab` per
/// row. The known-set is only advanced on a successful query, so an appearance
/// missed during a failed query is retried on the next snapshot.
pub(super) fn reconcile_tabs(app: &mut AppState, tabs: &[String], full: bool) {
    if app.view != TuiView::Worktrees || app.pin_cwd {
        return;
    }
    // A tab we previously issued has now surfaced in zellij's list — clear its
    // open-claim. This is what ends the suppression window: instead of guessing
    // how long zellij takes to register a tab (a flat timer that this monorepo's
    // ~5s git-refresh cadence raced straight past), we hold the claim until we
    // actually observe the tab, then release it. A later same-name worktree can
    // then reopen cleanly.
    app.recent_tab_opens
        .retain(|name, _| !tabs.iter().any(|t| t == name));
    for row in &app.snapshot.rows {
        if tabs.iter().any(|t| t == &row.name) {
            app.managed_tabs.insert(row.name.clone());
        }
    }
    let names: HashSet<String> = app
        .snapshot
        .rows
        .iter()
        .map(|row| row.name.clone())
        .collect();
    let eligible = eligible_for_open(app.known_worktrees.as_ref(), &names, full);
    app.known_worktrees = Some(names);
    // Collect first: opening a tab mutates `recent_tab_opens`, which can't
    // alias the `snapshot.rows` borrow held by the loop.
    let missing: Vec<(PathBuf, String)> = app
        .snapshot
        .rows
        .iter()
        .filter(|row| eligible.contains(&row.name) && !tabs.iter().any(|t| t == &row.name))
        .map(|row| (row.path.clone(), row.name.clone()))
        .collect();
    if !missing.is_empty() {
        // Log both sides so a spurious "missing" (a tab that exists but isn't
        // yet visible to `query-tab-names`) is distinguishable from a genuinely
        // absent tab when diagnosing duplicate-tab reports.
        let missing_names: Vec<&str> = missing.iter().map(|(_, n)| n.as_str()).collect();
        tracing::debug!(?tabs, missing = ?missing_names, full, "reconcile: opening tabs for new worktrees");
    }
    for (path, name) in missing {
        open_worktree_tab_debounced(app, &path, &name);
    }
}

/// Which worktree names may get a tab this round: everything on the first
/// reconcile (`prev` is `None`) or when a full reconcile was requested,
/// otherwise only names absent from the previous round. A name that vanishes
/// and returns (worktree deleted then recreated) counts as new again because
/// the caller replaces the known-set with `current` every round.
fn eligible_for_open(
    prev: Option<&HashSet<String>>,
    current: &HashSet<String>,
    full: bool,
) -> HashSet<String> {
    match prev {
        Some(prev) if !full => current.difference(prev).cloned().collect(),
        _ => current.clone(),
    }
}

/// Failsafe lifetime of an open-claim. A claim is normally cleared the moment
/// [`reconcile_tabs`] sees the tab appear in `query-tab-names`; this bound only
/// matters when an open *failed* (the tab never appears) — after it elapses we
/// allow a retry rather than suppressing forever. It must comfortably exceed the
/// worst-case `new-tab` → tab-registered latency, so it is intentionally far
/// longer than any single snapshot interval.
const TAB_OPEN_FAILSAFE: Duration = Duration::from_secs(30);

/// Open a worktree tab unless we already have an outstanding open-claim for the
/// same name. Both the targeted `pending_create` path and [`reconcile_tabs`]
/// route through here. A claim is held until [`reconcile_tabs`] observes the tab
/// in zellij's list (the normal release) or [`TAB_OPEN_FAILSAFE`] elapses (the
/// failed-open retry), so the freshly-opened tab is never reopened by the
/// snapshots that arrive before zellij registers it.
pub(super) fn open_worktree_tab_debounced(app: &mut AppState, path: &Path, name: &str) {
    let now = Instant::now();
    app.recent_tab_opens
        .retain(|_, t| now.duration_since(*t) < TAB_OPEN_FAILSAFE);
    if app.recent_tab_opens.contains_key(name) {
        tracing::debug!(worktree = %name, "tab open suppressed (claim outstanding)");
        return;
    }
    app.recent_tab_opens.insert(name.to_string(), now);
    app.managed_tabs.insert(name.to_string());
    tracing::info!(worktree = %name, "opening worktree tab");
    let path = path.to_path_buf();
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = crate::launch::open_worktree_tab(&path, &name) {
            tracing::warn!(worktree = %name, "open worktree tab failed: {e}");
        }
    });
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

    fn names(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Tab opens are driven by worktree *appearance*: everything qualifies on
    /// the first round, only additions afterwards, and `full` (manual refresh)
    /// re-qualifies everything. A worktree that disappears and returns is an
    /// appearance again.
    #[test]
    fn eligible_for_open_tracks_appearances() {
        // First reconcile: no previous set, everything is new.
        assert_eq!(
            eligible_for_open(None, &names(&["main", "feat"]), false),
            names(&["main", "feat"])
        );
        // Steady state: nothing new, nothing eligible — a closed tab for a
        // known worktree must NOT be resurrected by the next snapshot.
        assert_eq!(
            eligible_for_open(
                Some(&names(&["main", "feat"])),
                &names(&["main", "feat"]),
                false
            ),
            names(&[])
        );
        // Only the addition qualifies.
        assert_eq!(
            eligible_for_open(Some(&names(&["main"])), &names(&["main", "feat"]), false),
            names(&["feat"])
        );
        // Removals alone qualify nothing.
        assert_eq!(
            eligible_for_open(Some(&names(&["main", "feat"])), &names(&["main"]), false),
            names(&[])
        );
        // Full reconcile (manual refresh) re-qualifies everything known.
        assert_eq!(
            eligible_for_open(
                Some(&names(&["main", "feat"])),
                &names(&["main", "feat"]),
                true
            ),
            names(&["main", "feat"])
        );
        // Deleted-then-recreated: the caller replaced the known-set with the
        // post-deletion set, so the name's return is an appearance.
        assert_eq!(
            eligible_for_open(Some(&names(&["main"])), &names(&["main", "feat"]), false),
            names(&["feat"])
        );
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
}
