use super::client::{request_branches, send_refresh, send_update, subscribe_loop};
use super::ensure_daemon;
use super::input::{
    activate_worktree_tab, handle_create_key, handle_input_key, handle_mouse, spawn_close_tab,
};
use super::state::{AppState, CreatePicker, CreateStep, HitRegions, InputMode};
use super::view;
use crate::cli::TuiView;
use crate::daemon::resources;
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::kill;
use crate::worktree::BranchInfo;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

pub(super) enum AppEvent {
    Snapshot(Snapshot),
    Input(Event),
    Tick,
    Resources(resources::Snapshot),
    PrStatus(PrSnapshot),
    RefreshDone(Result<Vec<String>, String>),
    ZellijError(String),
    Disconnected(String),
    Connected,
    /// The default-branch update finished; `Ok(())` clears the status line,
    /// `Err` carries a message to surface.
    UpdateDone(Result<(), String>),
    /// The daemon's reply to a ListBranches request, for the open create picker.
    Branches(Vec<BranchInfo>),
    /// A create/delete request failed; surface the message in the footer.
    ActionError(String),
    InputError(String),
    /// A non-forced delete was refused; re-open the confirmation as a force
    /// override. Carries `(worktree_name, reason_description)`.
    DeleteNeedsForce(String, String),
}

pub(super) async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    common: &std::path::Path,
    repo_name: String,
    view: TuiView,
    cwd: PathBuf,
    pin_cwd: bool,
) -> Result<()>
where
    B::Error: Send + Sync + 'static,
{
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);

    // Daemon subscriber task.
    {
        let tx = tx.clone();
        let common = common.to_path_buf();
        let cwd = cwd.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = subscribe_loop(&common, tx.clone()).await {
                    tracing::debug!("subscriber: {e:?}");
                    let _ = tx.send(AppEvent::Disconnected(e.to_string())).await;
                }
                if let Err(e) = ensure_daemon(&cwd).await {
                    tracing::debug!("ensure daemon after disconnect: {e:?}");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }

    // Input pump (blocking poll on a thread).
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(evt) => {
                            if tx.blocking_send(AppEvent::Input(evt)).is_err() {
                                return;
                            }
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(250)),
                    },
                    Ok(false) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(250)),
                }
            }
        });
    }

    // Spinner ticker.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(120));
            loop {
                iv.tick().await;
                if tx.send(AppEvent::Tick).await.is_err() {
                    return;
                }
            }
        });
    }

    let mut app = AppState {
        snapshot: Snapshot { rows: vec![] },
        selected: None,
        worktree_scroll: 0,
        spinner_frame: 0,
        repo_name,
        view,
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
        current_dir: cwd.canonicalize().ok(),
        pin_cwd,
        tab_env: std::env::var("ZELLIJ_TAB_NAME")
            .ok()
            .filter(|s| !s.is_empty()),
        current_tab: None,
        regions: HitRegions::default(),
        last_click: None,
    };
    app.current_tab = app.tab_env.clone();

    terminal.draw(|f| view::render(f, &mut app))?;

    while let Some(evt) = rx.recv().await {
        match evt {
            AppEvent::Snapshot(s) => {
                app.snapshot = s;
                app.pin_snapshot();
                app.reconcile_selection();
                // When swamp itself removed a worktree, close its now-defunct
                // tab (its panes point at a deleted directory).
                if let Some(ref name) = app.pending_delete
                    && !app.snapshot.rows.iter().any(|r| &r.name == name)
                {
                    spawn_close_tab(tx.clone(), name.clone());
                    app.pending_delete = None;
                    app.status_msg = None;
                }
                // When swamp itself created a worktree, open and switch to its
                // tab. This is the only snapshot-driven open — every other
                // worktree tab is opened by explicit user activation, never by
                // a snapshot (tab pinning is gone).
                if let Some(name) = app.pending_create.clone() {
                    let created = app
                        .snapshot
                        .rows
                        .iter()
                        .find(|r| r.name == name)
                        .map(|r| (r.path.clone(), r.name.clone()));
                    if let Some((path, name)) = created {
                        activate_worktree_tab(tx.clone(), path, name);
                        app.pending_create = None;
                        app.status_msg = None;
                    }
                }
            }
            AppEvent::Tick => {
                let needs_tick = app.refreshing
                    || app.toast.is_some()
                    || app
                        .snapshot
                        .rows
                        .iter()
                        .any(|r| matches!(r.agent, crate::daemon::state::AgentStatus::Working));
                if !needs_tick {
                    continue;
                }
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
                if let Some((_, ticks)) = &mut app.toast {
                    *ticks = ticks.saturating_sub(1);
                    if *ticks == 0 {
                        app.toast = None;
                    }
                }
            }
            AppEvent::Resources(snap) => {
                app.resources = snap;
            }
            AppEvent::PrStatus(pr) => {
                app.pr_snapshot = pr;
            }
            AppEvent::RefreshDone(res) => {
                // Refresh only re-reads daemon status now; it never opens or
                // closes tabs. Open tabs are user state and are left untouched.
                app.refreshing = false;
                app.status_msg = res.err();
            }
            AppEvent::ZellijError(msg) => {
                app.status_msg = Some(msg);
            }
            AppEvent::Disconnected(msg) => {
                app.connected = false;
                app.status_msg = Some(format!("Disconnected from daemon: {msg}"));
            }
            AppEvent::Connected => {
                app.connected = true;
                if app
                    .status_msg
                    .as_ref()
                    .is_some_and(|msg| msg.starts_with("Disconnected from daemon:"))
                {
                    app.status_msg = None;
                }
            }
            AppEvent::UpdateDone(res) => {
                app.status_msg = res.err();
            }
            AppEvent::Branches(branches) => {
                if let Some(InputMode::Create(p)) = app.input.as_mut() {
                    p.loading = false;
                    if p.step == CreateStep::Base {
                        p.selected = branches.iter().position(|b| b.is_default).unwrap_or(0);
                    }
                    p.branches = branches;
                }
            }
            AppEvent::ActionError(msg) => {
                app.pending_create = None;
                app.pending_delete = None;
                app.status_msg = Some(msg);
            }
            AppEvent::InputError(msg) => {
                app.pending_create = None;
                app.pending_delete = None;
                app.input = None;
                app.status_msg = Some(msg);
            }
            AppEvent::DeleteNeedsForce(name, reason) => {
                // The daemon refused; re-prompt as a force override so the user
                // can decide whether to proceed.
                app.pending_delete = None;
                app.status_msg = None;
                app.input = Some(InputMode::ConfirmDelete {
                    name,
                    force_reason: Some(reason),
                });
            }
            AppEvent::Input(Event::Key(k)) => {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if matches!(app.input, Some(InputMode::Create(_))) {
                    handle_create_key(&mut app, k, &tx, common);
                    terminal.draw(|f| view::render(f, &mut app))?;
                    continue;
                }
                if let Some(mode) = app.input.take() {
                    handle_input_key(&mut app, mode, k, &tx, common);
                    terminal.draw(|f| view::render(f, &mut app))?;
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if app.view == TuiView::Resources {
                            let max = view::max_resource_scroll(
                                &app.resources,
                                app.resource_viewport_height,
                            );
                            app.resource_scroll = (app.resource_scroll + 1).min(max);
                        } else if !app.snapshot.rows.is_empty() {
                            app.move_selection(1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if app.view == TuiView::Resources {
                            app.resource_scroll = app.resource_scroll.saturating_sub(1);
                        } else {
                            app.move_selection(-1);
                        }
                    }
                    KeyCode::Char('g') => {
                        if app.view == TuiView::Resources {
                            app.resource_scroll = 0;
                        } else {
                            app.select_first();
                        }
                    }
                    KeyCode::Char('G') => {
                        if app.view == TuiView::Resources {
                            let max = view::max_resource_scroll(
                                &app.resources,
                                app.resource_viewport_height,
                            );
                            app.resource_scroll = max;
                        } else if !app.snapshot.rows.is_empty() {
                            app.select_last();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(row) = app.selected_row() {
                            activate_worktree_tab(tx.clone(), row.path.clone(), row.name.clone());
                        }
                    }
                    KeyCode::Char('c') => {
                        app.status_msg = None;
                        app.input = Some(InputMode::Create(CreatePicker {
                            step: CreateStep::Branch,
                            filter: String::new(),
                            branches: Vec::new(),
                            selected: 0,
                            scroll: 0,
                            new_branch: None,
                            loading: true,
                        }));
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            match request_branches(&common).await {
                                Ok(branches) => {
                                    let _ = tx.send(AppEvent::Branches(branches)).await;
                                }
                                Err(e) => {
                                    let _ = tx.send(AppEvent::InputError(e.to_string())).await;
                                }
                            }
                        });
                    }
                    KeyCode::Char('d') => {
                        if let Some(name) = app.selected_row().map(|row| row.name.clone()) {
                            app.status_msg = None;
                            // For the initial prompt there is no force_reason;
                            // if the daemon refuses, DeleteNeedsForce re-opens
                            // it with a reason.
                            app.input = Some(InputMode::ConfirmDelete {
                                name,
                                force_reason: None,
                            });
                        }
                    }
                    KeyCode::Char('h') => {
                        if let Some(name) = app.selected_row().map(|row| row.name.clone()) {
                            app.status_msg = None;
                            app.input = Some(InputMode::PickHarness { name });
                        }
                    }
                    KeyCode::Char('r') if !app.refreshing => {
                        app.refreshing = true;
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            if let Err(e) = send_refresh(&common, tx.clone()).await {
                                let _ = tx.send(AppEvent::RefreshDone(Err(e.to_string()))).await;
                                tracing::warn!("refresh: {e:?}");
                            }
                        });
                    }
                    KeyCode::Char('u') => {
                        app.status_msg = Some("Updating default branch…".into());
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            if let Err(e) = send_update(&common, tx.clone()).await {
                                let _ = tx.send(AppEvent::UpdateDone(Err(e.to_string()))).await;
                            }
                        });
                    }
                    KeyCode::Char('K') => {
                        return kill::run(Some(common.parent().unwrap_or(common).to_path_buf()));
                    }
                    _ => {}
                }
            }
            AppEvent::Input(Event::Mouse(m)) => handle_mouse(&mut app, m, &tx, common),
            AppEvent::Input(_) => {}
        }
        terminal.draw(|f| view::render(f, &mut app))?;
    }
    Ok(())
}
