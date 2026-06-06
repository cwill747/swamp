use super::client::{request_branches, send_refresh, send_update, subscribe_loop};
use super::input::{handle_create_key, handle_input_key, handle_mouse, reconcile_tabs};
use super::state::{AppState, CreatePicker, CreateStep, HitRegions, InputMode};
use super::view;
use crate::cli::TuiView;
use crate::daemon::resources;
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::kill;
use crate::worktree::BranchInfo;
use crate::zellij;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

pub(super) enum AppEvent {
    Snapshot(Snapshot),
    Input(Event),
    Tick,
    Resources(resources::Snapshot),
    PrStatus(PrSnapshot),
    RefreshDone(Vec<String>),
    /// The default-branch update finished; `Ok(())` clears the status line,
    /// `Err` carries a message to surface.
    UpdateDone(Result<(), String>),
    /// The daemon's reply to a ListBranches request, for the open create picker.
    Branches(Vec<BranchInfo>),
    /// A create/delete request failed; surface the message in the footer.
    ActionError(String),
    /// A non-forced delete was refused because the worktree is dirty; re-open
    /// the confirmation as a force override.
    DeleteNeedsForce(String),
}

pub(super) async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    common: &std::path::Path,
    repo_name: String,
    view: TuiView,
    cwd: PathBuf,
    pin_cwd: bool,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);

    // Daemon subscriber task.
    {
        let tx = tx.clone();
        let common = common.to_path_buf();
        tokio::spawn(async move {
            loop {
                if let Err(e) = subscribe_loop(&common, tx.clone()).await {
                    tracing::debug!("subscriber: {e:?}");
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
                if event::poll(Duration::from_millis(100)).unwrap_or(false)
                    && let Ok(evt) = event::read()
                    && tx.blocking_send(AppEvent::Input(evt)).is_err()
                {
                    return;
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
        selected: 0,
        spinner_frame: 0,
        repo_name,
        view,
        refreshing: false,
        pending_delete: None,
        pending_create: false,
        pre_create_names: HashSet::new(),
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
                if app.selected >= app.snapshot.rows.len() {
                    app.selected = app.snapshot.rows.len().saturating_sub(1);
                }
                if let Some(ref name) = app.pending_delete
                    && !app.snapshot.rows.iter().any(|r| &r.name == name)
                {
                    let _ = zellij::close_tab_by_name(name);
                    app.pending_delete = None;
                    app.status_msg = None;
                }
                if app.pending_create {
                    let new_rows: Vec<_> = app
                        .snapshot
                        .rows
                        .iter()
                        .filter(|r| !app.pre_create_names.contains(&r.name))
                        .collect();
                    if !new_rows.is_empty() {
                        for row in &new_rows {
                            let _ = crate::launch::open_worktree_tab(&row.path, &row.name);
                        }
                        if let Some(last) = new_rows.last() {
                            let _ = zellij::go_to_tab_name(&last.name);
                        }
                        app.pending_create = false;
                        app.status_msg = None;
                    }
                } else {
                    reconcile_tabs(&app);
                }
            }
            AppEvent::Tick => {
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
            AppEvent::RefreshDone(wt_names) => {
                app.refreshing = false;
                if let Ok(tabs) = zellij::list_tab_names() {
                    for tab in &tabs {
                        if tab == "dashboard" {
                            continue;
                        }
                        if !wt_names.iter().any(|n| n == tab) {
                            let _ = zellij::close_tab_by_name(tab);
                        }
                    }
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
                app.pending_create = false;
                app.pending_delete = None;
                app.input = None;
                app.status_msg = Some(msg);
            }
            AppEvent::DeleteNeedsForce(name) => {
                // The snapshot looked clean but the daemon found uncommitted
                // work; re-prompt as a force override instead of failing.
                app.pending_delete = None;
                app.status_msg = None;
                app.input = Some(InputMode::ConfirmDelete { name, dirty: true });
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
                            app.selected = (app.selected + 1).min(app.snapshot.rows.len() - 1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if app.view == TuiView::Resources {
                            app.resource_scroll = app.resource_scroll.saturating_sub(1);
                        } else {
                            app.selected = app.selected.saturating_sub(1);
                        }
                    }
                    KeyCode::Char('g') => {
                        if app.view == TuiView::Resources {
                            app.resource_scroll = 0;
                        } else {
                            app.selected = 0;
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
                            app.selected = app.snapshot.rows.len() - 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            let _ = zellij::go_to_tab_name(&row.name);
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
                                    let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                                }
                            }
                        });
                    }
                    KeyCode::Char('d') => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            app.status_msg = None;
                            let dirty =
                                row.staged + row.unstaged + row.untracked > 0 || row.conflict;
                            app.input = Some(InputMode::ConfirmDelete {
                                name: row.name.clone(),
                                dirty,
                            });
                        }
                    }
                    KeyCode::Char('h') => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            app.status_msg = None;
                            app.input = Some(InputMode::PickHarness {
                                name: row.name.clone(),
                            });
                        }
                    }
                    KeyCode::Char('r') if !app.refreshing => {
                        app.refreshing = true;
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            if let Err(e) = send_refresh(&common, tx).await {
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
