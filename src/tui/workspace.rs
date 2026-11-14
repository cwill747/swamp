//! Terminal lifecycle and side effects for workspace navigation surfaces.
use super::event::AppEvent;
use super::{AppState, CreatePicker, CreateStep, HitRegions, InputMode, client, input, view};
use crate::cli::TuiView;
use crate::daemon::{
    resources,
    state::{PrSnapshot, Snapshot},
};
use crate::overview::jump::{self, Target};
use crate::workspace::{
    Connection, DestinationKey,
    client::{ClientStatus, WorkspaceClient},
    protocol::ResourceState,
    seen::SeenLedger,
    selection,
    ui::{self, Action, App, Mode, Section},
};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton,
        MouseEvent, MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io::stdout,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet};

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            stdout(),
            crossterm::cursor::Show,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

fn local_state(cwd: &Path) -> AppState {
    AppState {
        snapshot: Snapshot::default(),
        selected: None,
        worktree_scroll: 0,
        spinner_frame: 0,
        repo_name: String::new(),
        view: TuiView::Worktrees,
        refreshing: false,
        pending_delete: None,
        pending_create: None,
        connected: false,
        input: None,
        status_msg: None,
        toast: None,
        resources: resources::Snapshot::default(),
        pr_snapshot: PrSnapshot::default(),
        resource_scroll: 0,
        resource_viewport_height: 0,
        current_dir: Some(cwd.to_path_buf()),
        pin_cwd: true,
        tab_env: std::env::var("ZELLIJ_TAB_NAME").ok(),
        current_tab: None,
        regions: HitRegions::default(),
        last_click: None,
    }
}

enum ResultEvent {
    Activation(Result<jump::Outcome, String>, Option<ActivationCapture>),
    Metadata(
        Result<crate::workspace::seen::SeenState, String>,
        Result<Option<selection::SharedSelection>, String>,
        u64,
    ),
    Status(Result<(), String>),
    SharedWrite(Result<(), String>),
}

#[derive(Clone)]
struct ActivationCapture {
    repo: String,
    session: String,
    row: crate::daemon::state::WorktreeRow,
}

/// Return the revision captured when a destination was activated, but only
/// after Zellij confirms that its generated agent pane has focus.
fn completion_to_ack(
    outcome: &Result<jump::Outcome, String>,
    captured: Option<ActivationCapture>,
) -> Option<ActivationCapture> {
    if matches!(outcome, Ok(jump::Outcome::AgentFocused)) {
        captured
    } else {
        None
    }
}

fn captured_target_still_matches(app: &App, captured: &ActivationCapture) -> bool {
    app.repositories.iter().any(|repo| {
        repo.id == captured.repo
            && repo
                .identity
                .as_ref()
                .is_some_and(|identity| identity.session_name == captured.session)
            && repo.destinations().iter().any(|destination| {
                destination.row.path == captured.row.path
                    && destination.row.branch == captured.row.branch
                    && destination.row.session_name == captured.row.session_name
            })
    })
}

fn apply_shared_update(
    app: &mut App,
    shared: selection::SharedSelection,
    last_shared: &mut Option<selection::SharedSelection>,
    write_pending: bool,
    have_inventory: bool,
) -> bool {
    if write_pending || last_shared.as_ref() == Some(&shared) {
        return false;
    }
    if have_inventory {
        app.apply_shared(shared.clone());
    }
    *last_shared = Some(shared);
    true
}

fn apply_metadata_selection(
    app: &mut App,
    shared: selection::SharedSelection,
    last_shared: &mut Option<selection::SharedSelection>,
    write_pending: bool,
    have_inventory: bool,
    request_generation: u64,
    current_generation: u64,
) -> bool {
    request_generation == current_generation
        && apply_shared_update(app, shared, last_shared, write_pending, have_inventory)
}

#[derive(Debug, PartialEq, Eq)]
enum SlotActivation {
    Allocate,
    Pinned(String),
    Navigate,
}

fn slot_activation(row: Option<&crate::daemon::state::WorktreeRow>) -> SlotActivation {
    match row.and_then(|row| row.slot_state) {
        Some(crate::worktree::SlotState::Free) => SlotActivation::Allocate,
        Some(crate::worktree::SlotState::Pinned) => SlotActivation::Pinned(
            row.and_then(|row| row.pin.clone())
                .unwrap_or_else(|| "user pinned".into()),
        ),
        _ => SlotActivation::Navigate,
    }
}

fn can_activate_from_workspace(app: &App) -> bool {
    app.connection.is_empty()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuitFlow {
    Exit,
    ReturnToTerminal,
}

fn quit_flow(view_mode: TuiView, inside_zellij: bool) -> QuitFlow {
    if view_mode == TuiView::Workspace || !inside_zellij {
        QuitFlow::Exit
    } else {
        QuitFlow::ReturnToTerminal
    }
}

fn prompt_target_for(
    local: &AppState,
    app: &App,
    local_id: Option<&str>,
) -> Option<DestinationKey> {
    let (name, branch) = match local.input.as_ref()? {
        InputMode::ConfirmDelete { name, .. } | InputMode::PickHarness { name } => {
            (Some(name.as_str()), None)
        }
        InputMode::EditTitle { branch, .. } => (None, Some(branch.as_str())),
        InputMode::Create(_) => return None,
    };
    app.repositories
        .iter()
        .find(|r| Some(r.id.as_str()) == local_id)?
        .destinations()
        .into_iter()
        .find(|d| {
            name.is_some_and(|name| d.row.name == name) || branch.is_some_and(|b| d.row.branch == b)
        })
        .map(|d| d.key)
}

pub(super) fn delete_check_target_matches(
    snapshot: &Snapshot,
    name: &str,
    path: &Path,
    branch: &str,
) -> bool {
    snapshot
        .rows
        .iter()
        .any(|row| row.name == name && row.path == path && row.branch == branch)
}

fn route_workspace_mouse(app: &mut App, mouse: MouseEvent, prompt_open: bool) -> Action {
    if prompt_open || matches!(app.mode, Mode::Help | Mode::More | Mode::ConfirmKill) {
        // The modal owns pointer input; never let a click through to a hidden
        // destination row. The create picker currently has keyboard controls.
        return Action::None;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, section)) = app
                .section_hits
                .iter()
                .find(|(r, _)| r.contains((mouse.column, mouse.row).into()))
                .cloned()
            {
                app.section = section;
                app.selected = None;
                app.index = 0;
                app.reconcile();
                Action::None
            } else if let Some((_, entry, chevron, section)) = app
                .hits
                .iter()
                .rev()
                .find(|(r, _, _, _)| r.contains((mouse.column, mouse.row).into()))
                .cloned()
            {
                app.section = section;
                app.selected = Some((entry.repo.clone(), entry.key.clone()));
                app.reconcile();
                if chevron {
                    app.toggle(None);
                    Action::None
                } else {
                    Action::Activate
                }
            } else {
                Action::None
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if mouse.kind == MouseEventKind::ScrollUp {
                -1
            } else {
                1
            };
            if app
                .detail_area
                .is_some_and(|r| r.contains((mouse.column, mouse.row).into()))
            {
                app.detail_scroll =
                    (app.detail_scroll as i32 + delta).clamp(0, u16::MAX as i32) as u16;
            } else if let Some((_, section)) = app
                .section_areas
                .iter()
                .find(|(r, _)| r.contains((mouse.column, mouse.row).into()))
            {
                app.section = *section;
                app.move_selection(delta);
            }
            Action::None
        }
        _ => Action::None,
    }
}

fn clear_mouse_hit_targets(app: &mut App) {
    app.hits.clear();
    app.section_hits.clear();
    app.section_areas.clear();
    app.detail_area = None;
}

pub async fn run(
    dir: Option<PathBuf>,
    view_mode: TuiView,
    pin_cwd: bool,
    embedded: bool,
) -> Result<()> {
    let cwd = dir.unwrap_or(std::env::current_dir()?);
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let common = crate::worktree::git_common_dir(&crate::worktree::resolve_git_dir(&cwd)).ok();
    let local_id = common.as_deref().map(crate::util::repo_id);
    if let Some(common) = &common {
        crate::logging::init(
            common,
            false,
            false,
            &crate::config::load_config()
                .map(|c| c.logging)
                .unwrap_or_default(),
        );
    }
    let runtime = crate::util::runtime_base_dir()?;
    let selection_path = std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|s| selection::path_for(&runtime, &s).ok());
    let ledger = SeenLedger::open_default().ok();
    crate::aggregate::spawn_detached();
    let broker = WorkspaceClient::start(runtime, None);
    let mut inventory = broker.state();
    let status = broker.status();
    let mut app = App {
        embedded: embedded && view_mode == TuiView::Sidebar,
        active_path: pin_cwd.then_some(cwd.clone()),
        ascii: crate::util::ascii_mode(),
        connection: "Connecting to workspace broker".into(),
        ..Default::default()
    };
    let mut saved_selection = if let Some(path) = selection_path.clone() {
        tokio::task::spawn_blocking(move || selection::read(&path))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
    } else {
        None
    };
    let mut applied_shared: Option<selection::SharedSelection> = None;
    let mut local = local_state(&cwd);
    let (action_tx, mut action_rx) = mpsc::channel(64);
    let mut tasks = JoinSet::<ResultEvent>::new();
    let mut resource_repo = None;
    let mut last_shared = None;
    let mut prompt_target: Option<DestinationKey> = None;
    let mut metadata_pending = false;
    let mut metadata_at = tokio::time::Instant::now();
    let mut have_inventory = false;
    let mut shared_write_pending = false;
    let mut selection_generation = 0u64;

    enable_raw_mode()?;
    let _guard = TerminalGuard;
    crossterm::execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut tick = tokio::time::interval(Duration::from_millis(60));
    let result = async {
        loop {
            tick.tick().await;
            if inventory.has_changed().unwrap_or(false) {
                let state = inventory.borrow_and_update().clone();
                if let Some(state) = state {
                    app.repositories = state.repositories;
                    have_inventory = true;
                    if let Some(shared) = last_shared.clone().or_else(|| saved_selection.take()) {
                        if applied_shared.as_ref() != Some(&shared) {
                            app.apply_shared(shared.clone());
                            applied_shared = Some(shared.clone());
                            last_shared = Some(shared);
                            selection_generation = selection_generation.wrapping_add(1);
                        } else {
                            app.reconcile();
                        }
                    } else {
                        app.reconcile();
                    }
                    if state.selected_repo == resource_repo {
                        app.resource_lines =
                            resource_text(&state.resource, resource_repo.as_deref());
                    }
                    if let Some(repo) = app
                        .repositories
                        .iter()
                        .find(|r| Some(&r.id) == local_id.as_ref())
                    {
                        local.snapshot = repo.snapshot.clone().unwrap_or_default();
                        local.pr_snapshot = repo.prs.clone().unwrap_or_default();
                        local.connected = repo.connection == Connection::Live;
                        local.pin_snapshot();
                    }
                }
            }
            app.connection = match &*status.borrow() {
                ClientStatus::Connected => String::new(),
                ClientStatus::Connecting => {
                    "Workspace disconnected; reconnecting (cached data is stale)".into()
                }
                ClientStatus::Unavailable(e) => format!("Workspace unavailable; retrying: {e}"),
            };
            while let Some(result) = tasks.try_join_next() {
                match result {
                    Ok(ResultEvent::Activation(result, captured)) => {
                        app.activating = false;
                        let captured = completion_to_ack(&result, captured);
                        match result {
                            Ok(jump::Outcome::AgentFocused) => {
                                app.status = "Agent focused".into();
                                if let (Some(ledger), Some(captured)) = (
                                    ledger.clone(),
                                    captured.filter(|c| captured_target_still_matches(&app, c)),
                                ) {
                                    tasks.spawn(async move {
                                        ResultEvent::Status(
                                            tokio::task::spawn_blocking(move || {
                                                ledger.acknowledge(&captured.repo, &captured.row)
                                            })
                                            .await
                                            .map_err(|e| e.to_string())
                                            .and_then(|r| r.map_err(|e| e.to_string())),
                                        )
                                    });
                                }
                            }
                            Ok(outcome) => {
                                app.status =
                                    format!("Partial navigation: {outcome:?}; Done remains unseen")
                            }
                            Err(e) => app.status = e,
                        }
                    }
                    Ok(ResultEvent::Metadata(seen, shared, request_generation)) => {
                        metadata_pending = false;
                        match seen {
                            Ok(seen) => app.seen = seen,
                            Err(e) => app.status = format!("Seen state unavailable: {e}"),
                        }
                        if let Ok(Some(shared)) = shared {
                            // Every generated view follows the same per-session
                            // selection. An in-flight local write owns the path
                            // until it completes; stale poll results are ignored.
                            if apply_metadata_selection(
                                &mut app,
                                shared,
                                &mut last_shared,
                                shared_write_pending,
                                have_inventory,
                                request_generation,
                                selection_generation,
                            ) {
                                selection_generation = selection_generation.wrapping_add(1);
                                if have_inventory {
                                    applied_shared = last_shared.clone();
                                }
                            }
                        } else if let Err(e) = shared {
                            app.status = format!("Selection state unavailable: {e}");
                        }
                    }
                    Ok(ResultEvent::Status(result)) => {
                        if let Err(e) = result {
                            app.status = e;
                        }
                        metadata_at = tokio::time::Instant::now();
                    }
                    Ok(ResultEvent::SharedWrite(result)) => {
                        shared_write_pending = false;
                        if let Err(e) = result {
                            app.status = e;
                            // Retry the latest app state after a failed atomic
                            // write instead of treating it as persisted.
                            last_shared = None;
                            selection_generation = selection_generation.wrapping_add(1);
                        }
                        metadata_at = tokio::time::Instant::now();
                    }
                    Err(e) => {
                        metadata_pending = false;
                        app.activating = false;
                        shared_write_pending = false;
                        last_shared = None;
                        selection_generation = selection_generation.wrapping_add(1);
                        app.status = format!("Workspace task: {e}");
                    }
                }
            }
            while let Ok(evt) = action_rx.try_recv() {
                match evt {
                    AppEvent::Branches(branches) => {
                        if let Some(InputMode::Create(p)) = &mut local.input {
                            p.loading = false;
                            p.branches = branches;
                            if p.step == CreateStep::Base {
                                p.selected =
                                    p.branches.iter().position(|b| b.is_default).unwrap_or(0);
                            }
                        }
                    }
                    AppEvent::DeleteNeedsForce {
                        name,
                        reason,
                        expected_branch,
                    } => {
                        if expected_branch.as_ref().is_some_and(|expected| {
                            !local
                                .snapshot
                                .rows
                                .iter()
                                .any(|row| row.name == name && &row.branch == expected)
                        }) {
                            app.status =
                                "Destination changed during delete; force was not offered".into();
                            continue;
                        }
                        local.pending_delete = None;
                        local.input = Some(InputMode::ConfirmDelete {
                            name,
                            force_reason: Some(reason),
                            close_tab: false,
                        });
                        prompt_target = prompt_target_for(&local, &app, local_id.as_deref());
                    }
                    AppEvent::DeleteTabCheckDone {
                        name,
                        path,
                        branch,
                        result,
                    } => {
                        if delete_check_target_matches(&local.snapshot, &name, &path, &branch) {
                            match result {
                                Ok(None) => {
                                    if let Some(common) = &common {
                                        crate::launch::spawn_delete_tab(
                                            &name,
                                            &path,
                                            common,
                                            false,
                                            Some(&branch),
                                        )
                                    }
                                }
                                Ok(Some(reason)) => {
                                    local.input = Some(InputMode::ConfirmDelete {
                                        name,
                                        force_reason: Some(reason),
                                        close_tab: true,
                                    });
                                    prompt_target =
                                        prompt_target_for(&local, &app, local_id.as_deref());
                                }
                                Err(e) => app.status = e,
                            }
                        } else {
                            app.status =
                                "Destination changed during delete check; tab remains open".into();
                        }
                    }
                    AppEvent::InputError(e) => {
                        app.status = e;
                        local.pending_create = None;
                        local.input = None;
                        prompt_target = None;
                    }
                    AppEvent::ActionError(e)
                    | AppEvent::ZellijError(e)
                    | AppEvent::Disconnected(e) => {
                        app.status = e;
                        local.pending_create = None;
                        local.pending_delete = None;
                        prompt_target = None;
                    }
                    AppEvent::RefreshDone(result) => {
                        local.refreshing = false;
                        if let Err(e) = result {
                            app.status = e;
                        } else {
                            app.status = "Refreshed".into();
                        }
                    }
                    AppEvent::UpdateDone(result) => {
                        app.status = result
                            .err()
                            .unwrap_or_else(|| "Default branch updated".into());
                    }
                    _ => {}
                }
            }
            if let Some(name) = local.pending_delete.clone()
                && !local.snapshot.rows.iter().any(|r| r.name == name)
            {
                input::spawn_close_tab(action_tx.clone(), name);
                local.pending_delete = None;
            }
            if let Some(branch) = local.pending_create.clone()
                && let Some(row) = local.snapshot.rows.iter().find(|r| r.branch == branch)
            {
                local.pending_create = None;
                if let Some(repo) = app
                    .repositories
                    .iter()
                    .find(|r| Some(&r.id) == local_id.as_ref())
                {
                    let entry = repo
                        .destinations()
                        .into_iter()
                        .find(|d| d.row.path == row.path);
                    if let Some(d) = entry {
                        app.selected = Some((repo.id.clone(), Some(d.key)));
                        app.section = Section::Spaces;
                        app.query.clear();
                        app.collapsed.remove(&repo.id);
                        app.reconcile();
                        activate(&mut app, &mut tasks);
                    }
                }
            }
            if !metadata_pending && metadata_at.elapsed() >= Duration::from_millis(500) {
                metadata_pending = true;
                metadata_at = tokio::time::Instant::now();
                let ledger = ledger.clone();
                let path = selection_path.clone();
                let request_generation = selection_generation;
                tasks.spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        ResultEvent::Metadata(
                            ledger
                                .map(|l| l.load().map_err(|e| e.to_string()))
                                .unwrap_or_else(|| Err("no safe state directory".into())),
                            path.map(|p| selection::read(&p).map_err(|e| e.to_string()))
                                .unwrap_or(Ok(None)),
                            request_generation,
                        )
                    })
                    .await
                    .unwrap_or_else(|e| {
                        ResultEvent::Metadata(Err(e.to_string()), Ok(None), request_generation)
                    })
                });
            }
            let selected_repo = if view_mode == TuiView::Sidebar {
                None
            } else {
                app.selected.as_ref().map(|s| s.0.clone())
            };
            if selected_repo != resource_repo {
                resource_repo = selected_repo.clone();
                app.resource_lines = vec!["Loading".into()];
                broker.select_repo(selected_repo).await?;
            }
            if have_inventory
                && view_mode != TuiView::Details
                && !shared_write_pending
                && let Some(path) = selection_path.clone()
            {
                let shared = app.shared();
                if last_shared.as_ref() != Some(&shared) {
                    last_shared = Some(shared.clone());
                    selection_generation = selection_generation.wrapping_add(1);
                    shared_write_pending = true;
                    tasks.spawn(async move {
                        ResultEvent::SharedWrite(
                            tokio::task::spawn_blocking(move || selection::write(&path, &shared))
                                .await
                                .map_err(|e| e.to_string())
                                .and_then(|r| r.map_err(|e| e.to_string())),
                        )
                    });
                }
            }
            while event::poll(Duration::ZERO)? {
                let action = match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if local.input.is_some() {
                            if let Some(target) = &prompt_target
                                && !app.repositories.iter().any(|r| {
                                    r.id == target.repo
                                        && r.destinations().iter().any(|d| d.key == target.parent())
                                })
                            {
                                local.input = None;
                                prompt_target = None;
                                app.status = "Destination changed; action cancelled".into();
                                continue;
                            }
                            if let Some(common) = &common {
                                if matches!(local.input, Some(InputMode::Create(_))) {
                                    input::handle_create_key(&mut local, key, &action_tx, common);
                                } else if let Some(mode) = local.input.take() {
                                    input::handle_input_key(
                                        &mut local, mode, key, &action_tx, common,
                                    );
                                }
                                if let Some(status) = local.status_msg.take() {
                                    app.status = status;
                                }
                            }
                            if local.input.is_none() {
                                prompt_target = None;
                            }
                            continue;
                        }
                        app.status.clear();
                        app.key(key)
                    }
                    Event::Mouse(mouse) => {
                        route_workspace_mouse(&mut app, mouse, local.input.is_some())
                    }
                    Event::Resize(_, _) => {
                        clear_mouse_hit_targets(&mut app);
                        // Render at the new size before routing another queued
                        // pointer event against geometry from the old frame.
                        break;
                    }
                    _ => Action::None,
                };
                match action {
                    Action::None => {}
                    Action::Quit => {
                        match quit_flow(view_mode, crate::zellij::in_zellij()) {
                            QuitFlow::Exit => return Ok(()),
                            QuitFlow::ReturnToTerminal => {}
                        }
                        let session = std::env::var("ZELLIJ_SESSION_NAME").unwrap_or_default();
                        tasks.spawn(async move {
                            ResultEvent::Status(
                                tokio::task::spawn_blocking(move || {
                                    crate::zellij::focus_active_terminal(&session, "dashboard")
                                })
                                .await
                                .map_err(|e| e.to_string())
                                .and_then(|r| r.map_err(|e| e.to_string())),
                            )
                        });
                    }
                    Action::Seen => {
                        if let (Some(ledger), Some(entry)) = (ledger.clone(), app.selected_entry())
                            && let Some(d) = entry.destination
                        {
                            let repo = entry.repo;
                            tasks.spawn(async move {
                                ResultEvent::Status(
                                    tokio::task::spawn_blocking(move || {
                                        ledger.acknowledge(&repo, &d.row)
                                    })
                                    .await
                                    .map_err(|e| e.to_string())
                                    .and_then(|r| r.map_err(|e| e.to_string())),
                                )
                            });
                        }
                    }
                    Action::Activate => {
                        let slot_action = app
                            .selected_entry()
                            .and_then(|e| e.destination)
                            .map(|d| slot_activation(Some(&d.row)))
                            .unwrap_or(SlotActivation::Navigate);
                        match slot_action {
                            SlotActivation::Allocate => local_action(
                                'c',
                                &mut app,
                                &mut local,
                                common.as_deref(),
                                local_id.as_deref(),
                                &action_tx,
                            ),
                            SlotActivation::Pinned(reason) => {
                                app.status = format!("Pinned slot is protected ({reason})");
                            }
                            SlotActivation::Navigate => activate(&mut app, &mut tasks),
                        }
                    }
                    Action::Local(key) => {
                        local_action(
                            key,
                            &mut app,
                            &mut local,
                            common.as_deref(),
                            local_id.as_deref(),
                            &action_tx,
                        );
                        prompt_target = prompt_target_for(&local, &app, local_id.as_deref());
                    }
                }
            }
            terminal.draw(|f| {
                ui::render(f, &mut app, view_mode);
                if local.input.is_some() {
                    view::render_overlay(f, &mut local);
                }
            })?;
        }
    }
    .await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    broker.close().await;
    result
}

fn activate(app: &mut App, tasks: &mut JoinSet<ResultEvent>) {
    if app.activating {
        app.status = "Opening destination…".into();
        return;
    }
    if !can_activate_from_workspace(app) {
        app.status = "Workspace disconnected; reconnect before opening a destination".into();
        return;
    }
    if !crate::zellij::in_zellij() {
        app.status = "Outside Zellij: use the displayed destination path".into();
        app.mode = Mode::Details;
        return;
    }
    let Some(entry) = app.selected_entry() else {
        return;
    };
    let Some(repo) = app.repositories.iter().find(|r| r.id == entry.repo) else {
        return;
    };
    if !repo.can_activate() {
        app.status = "Destination unavailable or unidentified; navigation disabled".into();
        return;
    }
    let Some(identity) = repo.identity.clone() else {
        return;
    };
    let (target, captured) = match entry.destination {
        Some(d) => (
            Target {
                common_dir: identity.common_dir,
                session: identity.session_name.clone(),
                worktree: d.row.name.clone(),
                path: d.row.path.clone(),
                expected_branch: Some(d.row.branch.clone()),
            },
            Some(ActivationCapture {
                repo: repo.id.clone(),
                session: identity.session_name,
                row: d.row,
            }),
        ),
        None => {
            let base = identity.common_dir.clone();
            let session = identity.session_name;
            app.activating = true;
            tasks.spawn(async move {
                ResultEvent::Activation(
                    tokio::task::spawn_blocking(move || {
                        crate::launch::ensure_background_session(&base)?;
                        crate::zellij::switch_session(&session, None)?;
                        Ok(jump::Outcome::SessionOnly {
                            reason: "Repository opened; select a worktree to visit its agent"
                                .into(),
                        })
                    })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r: anyhow::Result<_>| r.map_err(|e| e.to_string())),
                    None,
                )
            });
            return;
        }
    };
    app.activating = true;
    app.status = "Opening destination…".into();
    tasks.spawn(async move {
        ResultEvent::Activation(
            tokio::task::spawn_blocking(move || jump::jump(&jump::Zellij, &target))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string())),
            captured,
        )
    });
}

fn local_action(
    key: char,
    app: &mut App,
    local: &mut AppState,
    common: Option<&Path>,
    local_id: Option<&str>,
    tx: &mpsc::Sender<AppEvent>,
) {
    if key == 'O' {
        input::handle_open_overview_key(local, tx, crate::zellij::in_zellij());
        if let Some(s) = local.status_msg.take() {
            app.status = s;
        }
        return;
    }
    if app.selected.as_ref().map(|s| s.0.as_str()) != local_id || common.is_none() {
        app.status="This action supports the current repository only. Enter opens the selected repository.".into();
        return;
    }
    let common = common.unwrap();
    if !local.connected {
        app.status = "Local repository disconnected; action disabled".into();
        return;
    }
    local.selected = app
        .selected_entry()
        .and_then(|e| e.destination)
        .map(|d| d.row.name);
    match key {
        'c' => {
            local.input = Some(InputMode::Create(CreatePicker {
                step: CreateStep::Branch,
                filter: String::new(),
                branches: Vec::new(),
                selected: 0,
                scroll: 0,
                new_branch: None,
                loading: true,
            }));
            let common = common.to_path_buf();
            let tx = tx.clone();
            tokio::spawn(async move {
                match client::request_branches(&common).await {
                    Ok(b) => {
                        let _ = tx.send(AppEvent::Branches(b)).await;
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::InputError(e.to_string())).await;
                    }
                }
            });
        }
        'd' => input::handle_delete_key(local),
        'D' => input::handle_delete_current_tab_key(local),
        't' => {
            input::handle_title_key(local);
            if let Some(InputMode::EditTitle { text, .. }) = local.input.as_mut() {
                *text = ui::clean(text);
            }
        }
        'h' => {
            if let Some(name) = local.selected.clone() {
                local.input = Some(InputMode::PickHarness { name });
            }
        }
        'r' => {
            local.refreshing = true;
            let common = common.to_path_buf();
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = client::send_refresh(&common, tx.clone()).await {
                    let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                }
            });
        }
        'u' => {
            let common = common.to_path_buf();
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = client::send_update(&common, tx.clone()).await {
                    let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                }
            });
        }
        'K' => {
            let common = common.to_path_buf();
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    tokio::task::spawn_blocking(move || crate::kill::run(Some(common))).await
                {
                    let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                }
            });
        }
        _ => {}
    }
    if let Some(status) = local.status_msg.take() {
        app.status = status;
    }
}

fn resource_text(resource: &ResourceState, selected: Option<&str>) -> Vec<String> {
    match resource {
        ResourceState::Loading {
            repo_id, snapshot, ..
        } if Some(repo_id.as_str()) == selected => {
            stale_resource_text("Refreshing", snapshot.as_ref())
        }
        ResourceState::Unavailable {
            repo_id, snapshot, ..
        } if Some(repo_id.as_str()) == selected => {
            stale_resource_text("Unavailable from peer", snapshot.as_ref())
        }
        ResourceState::Disconnected {
            repo_id, snapshot, ..
        } if Some(repo_id.as_str()) == selected => {
            stale_resource_text("Disconnected", snapshot.as_ref())
        }
        ResourceState::Ready {
            repo_id, snapshot, ..
        } if Some(repo_id.as_str()) == selected => {
            let mut lines = vec![format!(
                "CPU {:.1}% / RSS {} / load {:.2}",
                snapshot.total_cpu,
                resources::fmt_bytes(snapshot.total_rss_bytes),
                snapshot.load1
            )];
            if snapshot.procs.is_empty() {
                lines.push("No session processes".into());
            }
            lines.extend(snapshot.procs.iter().take(12).map(|p| {
                format!(
                    "{} {:.1}% {} {}",
                    p.pid,
                    p.cpu,
                    resources::fmt_bytes(p.rss_kb * 1024),
                    p.comm
                )
            }));
            lines
        }
        _ => vec!["Loading selected repository".into()],
    }
}

fn stale_resource_text(label: &str, snapshot: Option<&resources::Snapshot>) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return vec![label.into()];
    };
    let mut lines = vec![format!("{label} (showing cached resources)")];
    lines.extend(resource_snapshot_text(snapshot));
    lines
}

fn resource_snapshot_text(snapshot: &resources::Snapshot) -> Vec<String> {
    let mut lines = vec![format!(
        "CPU {:.1}% / RSS {} / load {:.2}",
        snapshot.total_cpu,
        resources::fmt_bytes(snapshot.total_rss_bytes),
        snapshot.load1
    )];
    if snapshot.procs.is_empty() {
        lines.push("No session processes".into());
    }
    lines.extend(snapshot.procs.iter().take(12).map(|p| {
        format!(
            "{} {:.1}% {} {}",
            p.pid,
            p.cpu,
            resources::fmt_bytes(p.rss_kb * 1024),
            p.comm
        )
    }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::WorktreeRow;
    use crate::workspace::{
        Repository,
        ui::{App, Entry, Mode},
    };
    use crate::worktree::SlotState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    fn captured() -> ActivationCapture {
        let row: WorktreeRow = serde_json::from_value(serde_json::json!({
            "name":"work2", "path":"/repo/work2", "branch":"feature/a", "upstream":null,
            "ahead":0, "behind":0, "staged":0, "unstaged":0, "untracked":0,
            "conflict":false, "rebase":false, "agent":"idle", "agent_ts":10,
            "activity_id":"turn-2", "completion_id":"done-2", "session_name":"Agent"
        }))
        .unwrap();
        ActivationCapture {
            repo: "repo-1".into(),
            session: "repo-session".into(),
            row,
        }
    }

    fn test_row(slot_state: Option<SlotState>) -> WorktreeRow {
        let mut row = captured().row;
        row.name = "work1".into();
        row.branch = if slot_state == Some(SlotState::Free) {
            String::new()
        } else {
            "feature/a".into()
        };
        row.slot_state = slot_state;
        row.pin = (slot_state == Some(SlotState::Pinned)).then(|| "keep warm".into());
        row
    }

    fn test_repo(row: WorktreeRow) -> Repository {
        Repository {
            id: "repo-1".into(),
            label: "repo".into(),
            identity: None,
            socket: None,
            connection: Connection::Live,
            snapshot: Some(Snapshot {
                rows: vec![row],
                default_known: true,
            }),
            prs: None,
        }
    }

    fn click(row: u16, col: u16, kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn only_confirmed_agent_focus_returns_the_captured_revision_for_ack() {
        let captured = captured();
        let focused = Ok(jump::Outcome::AgentFocused);
        let returned = completion_to_ack(&focused, Some(captured.clone())).unwrap();
        assert_eq!(returned.repo, captured.repo);
        assert_eq!(returned.row.name, captured.row.name);
        assert_eq!(returned.row.activity_id, captured.row.activity_id);
        assert_eq!(returned.row.completion_id, captured.row.completion_id);

        for partial in [
            jump::Outcome::TabOnly {
                reason: "pane ambiguous".into(),
            },
            jump::Outcome::SessionOnly {
                reason: "tab missing".into(),
            },
        ] {
            assert!(completion_to_ack(&Ok(partial), Some(captured.clone())).is_none());
        }
        assert!(completion_to_ack(&Err("focus failed".into()), Some(captured)).is_none());
    }

    #[test]
    fn activation_ack_identity_rejects_reused_slot_but_allows_new_completion() {
        let capture = captured();
        let mut row = capture.row.clone();
        let mut repo = test_repo(row.clone());
        repo.identity = Some(crate::discover::RepoIdentity {
            common_dir: PathBuf::from("/repo"),
            session_name: capture.session.clone(),
            version: Some("test".into()),
        });
        let mut app = App {
            repositories: vec![repo],
            ..Default::default()
        };
        assert!(captured_target_still_matches(&app, &capture));

        // Completion metadata may advance while the logical destination key
        // stays the same. A successful focus still acknowledges only the
        // captured revision, leaving the newer completion unseen.
        row.activity_id = Some("turn-3".into());
        row.completion_id = Some("done-3".into());
        app.repositories[0].snapshot.as_mut().unwrap().rows[0] = row.clone();
        assert!(captured_target_still_matches(&app, &capture));

        row.branch = "reassigned".into();
        app.repositories[0].snapshot.as_mut().unwrap().rows[0] = row;
        assert!(!captured_target_still_matches(&app, &capture));
    }

    #[test]
    fn browsing_and_search_typing_do_not_request_seen_acknowledgement() {
        let mut app = App::default();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(app.key(key('j')), Action::None);
        assert_eq!(app.key(key('/')), Action::None);
        assert_eq!(app.mode, Mode::Search);
        assert_eq!(app.key(key('v')), Action::None, "v edits the focused query");
        assert_eq!(app.query, "v");
        app.mode = Mode::Navigation;
        assert_eq!(
            app.key(key('v')),
            Action::Seen,
            "Mark Seen remains explicit"
        );
        // Native Zellij tab navigation is delivered to Zellij, not this pane;
        // the only visit acknowledgement path is ResultEvent::Activation.
    }

    #[test]
    fn generated_views_follow_shared_selection_and_ignore_stale_poll_during_write() {
        let row: WorktreeRow = serde_json::from_value(serde_json::json!({
            "name":"work1", "path":"/repo/work1", "branch":"feature/a", "upstream":null,
            "ahead":0, "behind":0, "staged":0, "unstaged":0, "untracked":0,
            "conflict":false, "rebase":false, "agent":"idle", "agent_ts":10
        }))
        .unwrap();
        let repo = Repository {
            id: "repo".into(),
            label: "repo".into(),
            identity: None,
            socket: None,
            connection: Connection::Live,
            snapshot: Some(Snapshot {
                rows: vec![row],
                default_known: true,
            }),
            prs: None,
        };
        let destination = repo.destinations()[0].key.clone();
        let mut app = App {
            repositories: vec![repo],
            ..Default::default()
        };
        let mut last_shared = Some(selection::SharedSelection::default());
        let remote = selection::SharedSelection {
            destination: Some(destination),
            repository: Some("repo".into()),
            agents: true,
            query: "shared filter".into(),
            filter: "Waiting".into(),
            ..Default::default()
        };
        apply_shared_update(&mut app, remote.clone(), &mut last_shared, false, true);
        assert_eq!(app.query, "shared filter");
        assert_eq!(app.filter.label(), "Waiting");
        assert!(app.section == Section::Agents);

        app.query = "my newer local query".into();
        let stale = selection::SharedSelection {
            query: "stale poll result".into(),
            ..remote
        };
        apply_shared_update(&mut app, stale, &mut last_shared, true, true);
        assert_eq!(app.query, "my newer local query");

        let stale_read = selection::SharedSelection {
            query: "read before the local write".into(),
            ..last_shared.clone().unwrap()
        };
        last_shared = Some(selection::SharedSelection {
            query: "my newer local query".into(),
            ..Default::default()
        });
        assert!(!apply_metadata_selection(
            &mut app,
            stale_read,
            &mut last_shared,
            false,
            true,
            7,
            8,
        ));
        assert_eq!(app.query, "my newer local query");
    }

    #[test]
    fn workspace_mouse_routes_activation_but_blocks_underlying_rows_during_prompts() {
        let repo = test_repo(test_row(None));
        let destination = repo.destinations().remove(0);
        let entry = Entry {
            repo: repo.id.clone(),
            key: Some(destination.key.clone()),
            destination: Some(destination),
            primary: "Agent".into(),
            secondary: "repo".into(),
            state: crate::workspace::seen::SemanticState::Working,
            child: false,
            header: false,
        };
        let hit = Rect::new(2, 3, 12, 1);
        let mut app = App {
            repositories: vec![repo],
            ..Default::default()
        };
        app.hits.push((hit, entry.clone(), false, Section::Spaces));
        assert_eq!(
            route_workspace_mouse(
                &mut app,
                click(3, 4, MouseEventKind::Down(MouseButton::Left)),
                true
            ),
            Action::None
        );
        assert!(
            app.selected.is_none(),
            "modal clicks cannot activate hidden rows"
        );
        app.mode = Mode::Help;
        assert_eq!(
            route_workspace_mouse(
                &mut app,
                click(3, 4, MouseEventKind::Down(MouseButton::Left)),
                false
            ),
            Action::None,
            "help overlay blocks clicks on covered rows"
        );
        app.mode = Mode::Navigation;
        assert_eq!(
            route_workspace_mouse(
                &mut app,
                click(3, 4, MouseEventKind::Down(MouseButton::Left)),
                false
            ),
            Action::Activate
        );

        app.selected = None;
        app.hits[0].2 = true;
        assert_eq!(
            route_workspace_mouse(
                &mut app,
                click(3, 4, MouseEventKind::Down(MouseButton::Left)),
                false
            ),
            Action::None
        );
        assert!(
            app.collapsed.contains(&entry.repo),
            "chevron toggles instead of activating"
        );
        assert_eq!(
            route_workspace_mouse(&mut app, click(3, 4, MouseEventKind::ScrollDown), false),
            Action::None
        );
        let index = app.index;
        route_workspace_mouse(&mut app, click(40, 30, MouseEventKind::ScrollDown), false);
        assert_eq!(app.index, index, "wheel outside known panes is ignored");

        clear_mouse_hit_targets(&mut app);
        assert!(app.hits.is_empty());
        assert!(app.section_hits.is_empty());
        assert!(app.section_areas.is_empty());
        assert!(app.detail_area.is_none());
        assert_eq!(
            route_workspace_mouse(
                &mut app,
                click(3, 4, MouseEventKind::Down(MouseButton::Left)),
                false,
            ),
            Action::None,
            "old-frame coordinates cannot activate after resize invalidation"
        );
    }

    #[test]
    fn free_and_pinned_slots_never_open_detached_agent_tabs() {
        assert_eq!(
            slot_activation(Some(&test_row(Some(SlotState::Free)))),
            SlotActivation::Allocate
        );
        assert_eq!(
            slot_activation(Some(&test_row(Some(SlotState::Pinned)))),
            SlotActivation::Pinned("keep warm".into())
        );
        assert_eq!(
            slot_activation(Some(&test_row(Some(SlotState::Busy)))),
            SlotActivation::Navigate
        );
        assert_eq!(slot_activation(None), SlotActivation::Navigate);
    }

    #[test]
    fn prompt_identity_tracks_the_mutated_row_not_the_sidebar_selection() {
        let repo = test_repo(test_row(None));
        let key = repo.destinations()[0].key.clone();
        let app = App {
            repositories: vec![repo],
            selected: Some(("foreign".into(), None)),
            ..Default::default()
        };
        let mut local = local_state(Path::new("/repo/work1"));
        local.input = Some(InputMode::ConfirmDelete {
            name: "work1".into(),
            force_reason: None,
            close_tab: true,
        });
        assert_eq!(
            prompt_target_for(&local, &app, Some("repo-1")),
            Some(key.clone())
        );
        local.input = Some(InputMode::PickHarness {
            name: "work1".into(),
        });
        assert_eq!(
            prompt_target_for(&local, &app, Some("repo-1")),
            Some(key.clone())
        );
        local.input = Some(InputMode::EditTitle {
            branch: "feature/a".into(),
            text: String::new(),
        });
        assert_eq!(prompt_target_for(&local, &app, Some("repo-1")), Some(key));
        local.input = Some(InputMode::Create(CreatePicker {
            step: CreateStep::Branch,
            filter: String::new(),
            branches: Vec::new(),
            selected: 0,
            scroll: 0,
            new_branch: None,
            loading: false,
        }));
        assert!(prompt_target_for(&local, &app, Some("repo-1")).is_none());
        let snapshot = app.repositories[0].snapshot.as_ref().unwrap();
        let target = &snapshot.rows[0];
        assert!(delete_check_target_matches(
            snapshot,
            "work1",
            &target.path,
            &target.branch
        ));
        assert!(!delete_check_target_matches(
            snapshot,
            "work1",
            &target.path,
            "reassigned"
        ));
    }

    #[test]
    fn disconnected_workspace_cannot_activate_cached_destinations() {
        let mut app = App {
            connection: "Workspace disconnected; reconnecting".into(),
            ..Default::default()
        };
        assert!(!can_activate_from_workspace(&app));
        app.connection.clear();
        assert!(can_activate_from_workspace(&app));
    }

    #[test]
    fn foreign_mutation_is_rejected_before_it_changes_local_action_state() {
        let mut app = App {
            selected: Some(("foreign".into(), None)),
            ..Default::default()
        };
        let mut local = local_state(Path::new("/tmp"));
        local.connected = true;
        let (tx, _rx) = mpsc::channel(1);
        local_action(
            'd',
            &mut app,
            &mut local,
            Some(Path::new("/repo")),
            Some("current"),
            &tx,
        );
        assert!(app.status.contains("current repository only"));
        assert!(local.input.is_none());
        assert!(local.pending_delete.is_none());
    }

    #[test]
    fn title_editor_normalizes_reported_text_without_changing_branch_target() {
        let mut app = crate::workspace::ui::tests::fixture();
        app.repositories[0].snapshot.as_mut().unwrap().rows[1].title =
            "\x1b[31mMy title\x1b[0m".into();
        app.move_selection(1);
        let mut local = local_state(Path::new("/repo"));
        local.connected = true;
        local.snapshot = app.repositories[0].snapshot.clone().unwrap();
        let (tx, _rx) = mpsc::channel(1);
        local_action(
            't',
            &mut app,
            &mut local,
            Some(Path::new("/repo")),
            Some("swamp"),
            &tx,
        );
        assert!(
            matches!(local.input, Some(InputMode::EditTitle { branch, text })
            if branch == "herdr-ify" && text == "My title")
        );
    }

    #[test]
    fn quit_exits_or_returns_to_terminal_without_killing_sessions() {
        assert_eq!(quit_flow(TuiView::Workspace, true), QuitFlow::Exit);
        assert_eq!(quit_flow(TuiView::Sidebar, false), QuitFlow::Exit);
        assert_eq!(
            quit_flow(TuiView::Sidebar, true),
            QuitFlow::ReturnToTerminal
        );
    }
}
