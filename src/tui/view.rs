use super::icons;
use super::theme::Theme;
use super::{AppState, PrHit};
use crate::daemon::resources;
use crate::cli::TuiView;
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::github::{CheckState, PrSummary, ReviewDecision};
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use crate::tui::{CreateEntry, CreatePicker, CreateStep, InputMode};
use crate::util::ascii_mode;
use crate::worktree::BranchKind;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table};
use ratatui::Frame;
use std::time::{Duration, SystemTime};

pub fn render(f: &mut Frame, app: &mut AppState) {
    // Hit regions are rebuilt each frame; panels not drawn this frame stay None.
    app.regions = super::HitRegions::default();
    match app.view {
        TuiView::All => render_all(f, app),
        _ => render_single_panel(f, app),
    }

    // The create picker floats above whatever view is active.
    if matches!(app.input, Some(InputMode::Create(_))) {
        render_create_picker(f, app);
    }
}

/// Inner content area of a fully-bordered block (the row region).
fn bordered_inner(area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    Block::default().borders(Borders::ALL).inner(area)
}

fn render_single_panel(f: &mut Frame, app: &mut AppState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    match app.view {
        TuiView::Worktrees => render_worktree_table(f, app, chunks[0]),
        TuiView::AiStatus => render_ai_status(f, app, chunks[0]),
        TuiView::Resources => render_resources(f, app, chunks[0]),
        TuiView::PrStatus => render_pr_status(f, app, chunks[0]),
        TuiView::All => unreachable!(),
    }

    render_footer(f, app, chunks[1]);
}

fn render_all(f: &mut Frame, app: &mut AppState) {
    let area = f.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // main content
            Constraint::Length(1), // footer
        ])
        .split(area);

    // Header.
    let header = Line::from(vec![
        Span::styled(app.repo_name.clone(), Theme::accent_bold()),
        Span::raw("  "),
        Span::styled(
            format!("({} worktrees)", app.snapshot.rows.len()),
            Theme::muted(),
        ),
    ]);
    f.render_widget(Paragraph::new(header), outer[0]);

    // Main content: two columns.
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[1]);

    // Left column: worktree table (top) + resources (bottom).
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(8)])
        .split(columns[0]);

    // Right column: AI status (top) + PR & CI status (bottom).
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(8)])
        .split(columns[1]);

    render_worktree_table(f, app, left[0]);
    render_resources(f, app, left[1]);
    render_ai_status(f, app, right[0]);
    render_pr_status(f, app, right[1]);

    render_footer(f, app, outer[2]);
}

fn render_footer(f: &mut Frame, app: &AppState, area: ratatui::layout::Rect) {
    // The delete confirmation takes over the footer. (The create picker is a
    // centered overlay drawn separately in `render`.)
    if let Some(crate::tui::InputMode::ConfirmDelete { name, dirty }) = &app.input {
        let line = if *dirty {
            Line::from(vec![
                Span::styled("Worktree ", Style::default().fg(Theme::DIRTY)),
                Span::styled(format!("'{name}'"), Style::default().fg(Theme::DIRTY)),
                Span::styled(
                    " has uncommitted changes — force delete?",
                    Style::default().fg(Theme::DIRTY),
                ),
                Span::styled(" (y/n)", Theme::muted()),
            ])
        } else {
            Line::from(vec![
                Span::styled("Delete worktree ", Style::default().fg(Theme::DIRTY)),
                Span::styled(format!("'{name}'"), Style::default().fg(Theme::DIRTY)),
                Span::styled("? (y/n)", Theme::muted()),
            ])
        };
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    if let Some(msg) = &app.status_msg {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(Theme::DIRTY),
            ))),
            area,
        );
        return;
    }
    let mut spans = Vec::new();
    if app.refreshing {
        let frame = icons::SPINNER_FRAMES[app.spinner_frame % icons::SPINNER_FRAMES.len()];
        spans.push(Span::styled(
            format!("{frame} Refreshing… "),
            Style::default().fg(Theme::WORKING),
        ));
    }
    spans.push(Span::styled(
        "j/k move · enter jump · c create · d delete · r refresh · K kill · q quit",
        Theme::muted(),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_worktree_table(f: &mut Frame, app: &mut AppState, area: ratatui::layout::Rect) {
    let now = now_unix();
    let current_tab = app.current_tab.as_deref();
    let pin_current = app.view == TuiView::Worktrees;

    let rows: Vec<Row> = app
        .snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| build_row(i, r, app, now, current_tab, pin_current))
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),  // # + caret
            Constraint::Length(2),  // agent icon
            Constraint::Length(1),  // PR icon
            Constraint::Min(8),     // worktree name
            Constraint::Min(10),    // branch
            Constraint::Min(12),    // git
            Constraint::Length(5),  // age
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Theme::MUTED))
            .title(Span::styled(" Worktrees ", Theme::accent_bold())),
    )
    .column_spacing(1);

    f.render_widget(table, area);

    let inner = bordered_inner(area);
    let visible = (app.snapshot.rows.len()).min(inner.height as usize);
    app.regions.worktrees = Some((inner, visible));
}

fn render_ai_status(f: &mut Frame, app: &mut AppState, area: ratatui::layout::Rect) {
    let inner = bordered_inner(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(Span::styled(
            " AI Status ",
            Style::default()
                .fg(Theme::WORKING)
                .add_modifier(Modifier::BOLD),
        ));

    let now = now_unix();
    let recent_threshold = now.saturating_sub(300);

    // Collect rows with active or recently-idle agents, sorted by priority.
    let mut entries: Vec<&WorktreeRow> = app
        .snapshot
        .rows
        .iter()
        .filter(|r| {
            r.agent == AgentStatus::Working
                || r.agent == AgentStatus::Waiting
                || (r.agent == AgentStatus::Idle && r.agent_ts >= recent_threshold)
        })
        .collect();

    if entries.is_empty() {
        let lines = vec![Line::from(Span::styled(
            "No active sessions",
            Theme::muted(),
        ))];
        f.render_widget(Paragraph::new(lines).block(block), area);
        drop(entries);
        app.regions.ai = Some((inner, Vec::new()));
        return;
    }

    entries.sort_by_key(|r| {
        let priority = match r.agent {
            AgentStatus::Waiting => 0,
            AgentStatus::Working => 1,
            AgentStatus::Idle => 2,
        };
        (priority, r.name.clone())
    });

    let rows: Vec<Row> = entries
        .iter()
        .map(|r| {
            let is_recent = now.saturating_sub(r.agent_ts) < 300;

            // Status icon with spinner for working.
            let (icon, icon_style) = match r.agent {
                AgentStatus::Working => (
                    icons::agent_icon(r.agent, app.spinner_frame, true),
                    Style::default().fg(Theme::WORKING),
                ),
                AgentStatus::Waiting => (
                    icons::agent_icon(r.agent, app.spinner_frame, true),
                    Style::default()
                        .fg(Theme::WAITING)
                        .add_modifier(Modifier::BOLD),
                ),
                AgentStatus::Idle => (
                    icons::agent_icon(r.agent, app.spinner_frame, is_recent),
                    Style::default().fg(Theme::IDLE_RECENT),
                ),
            };
            let icon_cell = Cell::from(Span::styled(icon, icon_style));

            // Status label.
            let (label, label_style) = match r.agent {
                AgentStatus::Working => (
                    "working",
                    Style::default()
                        .fg(Theme::WORKING)
                        .add_modifier(Modifier::BOLD),
                ),
                AgentStatus::Waiting => (
                    "waiting",
                    Style::default()
                        .fg(Theme::WAITING)
                        .add_modifier(Modifier::BOLD),
                ),
                AgentStatus::Idle => ("done", Style::default().fg(Theme::IDLE_RECENT)),
            };
            let status_cell = Cell::from(Span::styled(label, label_style));

            // Worktree name.
            let name_cell =
                Cell::from(Span::styled(r.name.as_str(), Style::default().fg(Color::White)));

            // Branch.
            let branch_cell = Cell::from(Span::styled(
                truncate(&r.branch, 20),
                Style::default().fg(Theme::BRANCH),
            ));

            // Git status.
            let git_cell = Cell::from(Line::from(git_spans(r)));

            // Session name (Claude conversation title).
            let session_cell = Cell::from(Span::styled(
                r.session_name
                    .as_deref()
                    .map(|s| truncate(s, 30))
                    .unwrap_or_default(),
                Theme::muted(),
            ));

            // Elapsed time.
            let elapsed = if r.agent_ts > 0 {
                let dur = SystemTime::now()
                    .duration_since(unix_to_systemtime(r.agent_ts))
                    .unwrap_or(Duration::ZERO);
                format_compact_age(dur)
            } else {
                "-".to_string()
            };
            let time_style = if is_recent {
                Style::default().fg(Color::White)
            } else {
                Theme::muted()
            };
            let time_cell = Cell::from(Span::styled(elapsed, time_style));

            Row::new(vec![
                icon_cell,
                status_cell,
                name_cell,
                branch_cell,
                git_cell,
                time_cell,
                session_cell,
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // icon
            Constraint::Length(8),  // status label
            Constraint::Min(8),    // worktree name
            Constraint::Min(10),   // branch
            Constraint::Min(8),    // git
            Constraint::Length(6), // elapsed
            Constraint::Fill(1),   // session name
        ],
    )
    .block(block)
    .column_spacing(1);

    f.render_widget(table, area);

    // Map each visible AI row back to its worktree's snapshot index.
    let mut indices: Vec<usize> = entries
        .iter()
        .filter_map(|e| app.snapshot.rows.iter().position(|r| r.name == e.name))
        .collect();
    drop(entries);
    indices.truncate(inner.height as usize);
    app.regions.ai = Some((inner, indices));
}

fn render_resources(f: &mut Frame, app: &mut AppState, area: ratatui::layout::Rect) {
    let res = &app.resources;
    let title = Span::styled(
        " Resources ",
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(title);

    app.regions.resources = Some(area);

    if res.session_pid.is_none() && res.procs.is_empty() {
        let lines = vec![Line::from(Span::styled(
            "sampling…",
            Theme::muted(),
        ))];
        f.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Header row 1: session totals.
    let session_label = match res.session_pid {
        Some(pid) => format!("session pid {pid}"),
        None => "session: not found".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled("CPU ", Theme::muted()),
        Span::styled(
            format!("{:>5.1}%", res.total_cpu),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("RSS ", Theme::muted()),
        Span::styled(
            format!("{:>6}", resources::fmt_bytes(res.total_rss_bytes)),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("time ", Theme::muted()),
        Span::styled(
            resources::fmt_duration(res.total_user_time_secs),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled(format!("({} procs · {session_label})", res.procs.len()), Theme::muted()),
    ]));

    // Header row 2: system memory + load.
    let mem_pct = if res.mem_total_bytes > 0 {
        (res.mem_used_bytes as f64 / res.mem_total_bytes as f64) * 100.0
    } else {
        0.0
    };
    lines.push(Line::from(vec![
        Span::styled("mem ", Theme::muted()),
        Span::styled(
            format!(
                "{}/{} ({:.0}%)",
                resources::fmt_bytes(res.mem_used_bytes),
                resources::fmt_bytes(res.mem_total_bytes),
                mem_pct
            ),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("load ", Theme::muted()),
        Span::styled(
            format!("{:.2} {:.2} {:.2}", res.load1, res.load5, res.load15),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(""));

    // Column header.
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:>6}  {:>5}  {:>6}  {:>7}  ", "PID", "CPU%", "RSS", "TIME"),
            Theme::muted(),
        ),
        Span::styled("COMMAND", Theme::muted()),
    ]));

    for p in &res.procs {
        lines.push(Line::from(vec![
            Span::raw(format!("{:>6}  ", p.pid)),
            Span::styled(
                format!("{:>5.1}  ", p.cpu),
                if p.cpu > 10.0 {
                    Style::default().fg(Theme::WORKING).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::raw(format!("{:>6}  ", resources::fmt_bytes(p.rss_kb * 1024))),
            Span::raw(format!("{:>7}  ", resources::fmt_duration(p.time_secs))),
            Span::styled(p.comm.clone(), Style::default().fg(Color::White)),
        ]));
    }

    let viewport_height = area.height.saturating_sub(2);
    app.resource_viewport_height = viewport_height;
    let max = max_resource_scroll(&app.resources, viewport_height);
    app.resource_scroll = app.resource_scroll.min(max);

    f.render_widget(
        Paragraph::new(lines).block(block).scroll((app.resource_scroll, 0)),
        area,
    );
}

pub fn resource_content_height(res: &resources::Snapshot) -> u16 {
    if res.session_pid.is_none() && res.procs.is_empty() {
        return 0;
    }
    (4 + res.procs.len()) as u16
}

pub fn max_resource_scroll(res: &resources::Snapshot, viewport_height: u16) -> u16 {
    let content = resource_content_height(res);
    content.saturating_sub(viewport_height)
}

fn render_pr_status(f: &mut Frame, app: &mut AppState, area: ratatui::layout::Rect) {
    let inner = bordered_inner(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(Span::styled(
            " PR & CI Status ",
            Style::default()
                .fg(Theme::BRANCH)
                .add_modifier(Modifier::BOLD),
        ));

    let mut pr_rows: Vec<(&str, &PrSummary)> = Vec::new();
    for row in &app.snapshot.rows {
        if let Some(pr) = app.pr_snapshot.prs.get(&row.branch) {
            pr_rows.push((&row.branch, pr));
        }
    }

    if pr_rows.is_empty() {
        let lines = vec![Line::from(Span::styled(
            "No PRs for any worktree branch",
            Theme::muted(),
        ))];
        f.render_widget(Paragraph::new(lines).block(block), area);
        app.regions.prs = Some((inner, Vec::new()));
        return;
    }

    pr_rows.sort_by(|a, b| {
        let a_open = a.1.state == "OPEN";
        let b_open = b.1.state == "OPEN";
        b_open.cmp(&a_open).then(b.1.number.cmp(&a.1.number))
    });

    let max_rows = area.height.saturating_sub(2) as usize;
    let mut rows: Vec<Row> = Vec::new();
    for (branch, pr) in pr_rows.iter().take(max_rows) {
        let (icon, color) = pr_state_icon_color(pr);
        let state_cell = Cell::from(Span::styled(icon, Style::default().fg(color)));

        let number_cell = Cell::from(Span::styled(
            format!("#{}", pr.number),
            Style::default().fg(color),
        ));

        let checks_cell = if let Some(ref checks) = pr.checks {
            let (check_icon, check_color) =
                check_state_icon_color(checks, app.spinner_frame);
            let mut spans = vec![Span::styled(check_icon, Style::default().fg(check_color))];
            match checks {
                CheckState::Failure { passed, total }
                | CheckState::Pending { passed, total } => {
                    spans.push(Span::styled(
                        format!(" {}/{}", passed, total),
                        Style::default().fg(check_color),
                    ));
                }
                CheckState::Success => {}
            }
            Cell::from(Line::from(spans))
        } else {
            Cell::from("")
        };

        let review_cell = review_status_cell(&pr.review);

        let branch_cell = Cell::from(Span::styled(
            truncate(branch, 20),
            Style::default().fg(Theme::BRANCH),
        ));

        let title_cell = Cell::from(Span::styled(truncate(&pr.title, 40), Theme::muted()));

        rows.push(Row::new(vec![
            state_cell,
            number_cell,
            checks_cell,
            review_cell,
            branch_cell,
            title_cell,
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // PR state icon
            Constraint::Length(7), // #number
            Constraint::Length(7), // checks
            Constraint::Length(2), // review
            Constraint::Min(10),   // branch
            Constraint::Fill(1),   // title
        ],
    )
    .block(block)
    .column_spacing(1);

    f.render_widget(table, area);

    // One PrHit per visible row, in the same order as `rows` above.
    let hits: Vec<PrHit> = pr_rows
        .iter()
        .take(max_rows)
        .map(|(_, pr)| PrHit { url: pr.url.clone() })
        .collect();
    drop(pr_rows);
    app.regions.prs = Some((inner, hits));
}

fn pr_state_icon_color(pr: &PrSummary) -> (&'static str, Color) {
    let icon = icons::pr_icon(&pr.state, pr.is_draft);
    let color = if pr.is_draft {
        Color::DarkGray
    } else {
        match pr.state.as_str() {
            "OPEN" => Color::Green,
            "MERGED" => Color::Magenta,
            "CLOSED" => Color::Red,
            _ => Color::DarkGray,
        }
    };
    (icon, color)
}

fn review_status_cell<'a>(review: &Option<ReviewDecision>) -> Cell<'a> {
    match review {
        Some(ReviewDecision::Approved) => Cell::from(Span::styled(
            icons::review_approved(),
            Style::default().fg(Color::Green),
        )),
        Some(ReviewDecision::ChangesRequested) => Cell::from(Span::styled(
            icons::review_changes(),
            Style::default().fg(Color::Red),
        )),
        Some(ReviewDecision::Commented) => Cell::from(Span::styled(
            icons::review_commented(),
            Style::default().fg(Color::Yellow),
        )),
        _ => Cell::from(""),
    }
}

fn check_state_icon_color(checks: &CheckState, spinner_frame: usize) -> (String, Color) {
    match checks {
        CheckState::Success => (icons::check_success().to_string(), Color::Green),
        CheckState::Failure { .. } => (icons::check_failure().to_string(), Color::Red),
        CheckState::Pending { .. } => {
            let frame =
                icons::SPINNER_FRAMES[spinner_frame % icons::SPINNER_FRAMES.len()];
            (frame.to_string(), Color::Cyan)
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = max.saturating_sub(1);
        format!("{}…", &s[..end])
    }
}

fn build_row<'a>(
    i: usize,
    r: &'a WorktreeRow,
    app: &AppState,
    now: u64,
    current_tab: Option<&str>,
    pin_current: bool,
) -> Row<'a> {
    let is_current = current_tab.map(|t| t == r.name).unwrap_or(false);
    let is_pinned = pin_current && is_current;
    let recent = now.saturating_sub(r.agent_ts) < 300;

    let idx_cell = {
        let mut spans = Vec::new();
        if is_pinned {
            spans.push(Span::styled(
                format!("{}{} ", icons::current_marker(), i + 1),
                Style::default()
                    .fg(Theme::ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ));
        } else if is_current {
            spans.push(Span::styled(
                format!("{}{} ", icons::current_marker(), i + 1),
                Theme::accent_bold(),
            ));
        } else {
            spans.push(Span::styled(format!("  {}", i + 1), Theme::muted()));
        }
        Cell::from(Line::from(spans))
    };

    let agent_cell = {
        let glyph = icons::agent_icon(r.agent, app.spinner_frame, recent);
        let style = match r.agent {
            AgentStatus::Working => Style::default().fg(Theme::WORKING),
            AgentStatus::Waiting => Style::default()
                .fg(Theme::WAITING)
                .add_modifier(Modifier::BOLD),
            AgentStatus::Idle if recent => Style::default().fg(Theme::IDLE_RECENT),
            AgentStatus::Idle => Style::default().fg(Theme::IDLE_STALE),
        };
        Cell::from(Span::styled(glyph, style))
    };

    let pr_cell = if let Some(pr) = app.pr_snapshot.prs.get(&r.branch) {
        let (icon, color) = if pr.state == "OPEN" && !pr.is_draft {
            match &pr.review {
                Some(ReviewDecision::ChangesRequested) => {
                    (icons::review_changes(), Color::Red)
                }
                Some(ReviewDecision::Commented) => {
                    (icons::review_commented(), Color::Yellow)
                }
                Some(ReviewDecision::Approved) => {
                    (icons::review_approved(), Color::Green)
                }
                _ => pr_state_icon_color(pr),
            }
        } else {
            pr_state_icon_color(pr)
        };
        Cell::from(Span::styled(icon, Style::default().fg(color)))
    } else {
        Cell::from(Span::raw(" "))
    };

    let name_style = if is_pinned {
        Style::default()
            .fg(Theme::ACCENT)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else if is_current {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let name_cell = Cell::from(Span::styled(&r.name, name_style));

    let branch_cell = Cell::from(Span::styled(&r.branch, Style::default().fg(Theme::BRANCH)));

    let git_cell = Cell::from(Line::from(git_spans(r)));

    let age_cell = {
        let txt = if r.agent_ts == 0 {
            "-".to_string()
        } else {
            let age = SystemTime::now()
                .duration_since(unix_to_systemtime(r.agent_ts))
                .unwrap_or(Duration::ZERO);
            format_compact_age(age)
        };
        let style = if recent {
            Style::default().fg(Color::White)
        } else {
            Theme::muted()
        };
        Cell::from(Span::styled(txt, style))
    };

    let row = Row::new(vec![idx_cell, agent_cell, pr_cell, name_cell, branch_cell, git_cell, age_cell]);
    if i == app.selected {
        row.style(Theme::selected())
    } else if is_current {
        row.style(Theme::current())
    } else {
        row
    }
}

/// A centered rect sized `pct_w`% × `pct_h`% of `area`.
fn centered_rect(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let w = (area.width * pct_w / 100).clamp(1, area.width);
    let h = (area.height * pct_h / 100).clamp(1, area.height);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// The kind/remote label shown next to a branch row, e.g. `(local, default)`.
fn branch_label(b: &crate::worktree::BranchInfo) -> String {
    match (&b.kind, &b.remote) {
        (BranchKind::Remote, Some(r)) => format!("({r})"),
        (BranchKind::Remote, None) => "(remote)".to_string(),
        (BranchKind::Local, _) if b.is_default => "(local, default)".to_string(),
        (BranchKind::Local, _) => "(local)".to_string(),
    }
}

/// Build the visible list rows for the picker, adjusting `scroll` to keep the
/// selection in view. Returns `(rows, new_scroll)`.
fn picker_rows(p: &CreatePicker, list_height: usize) -> (Vec<Row<'static>>, usize) {
    let entries = p.entries();
    let n = entries.len();

    // Keep the selected entry within the visible window.
    let mut scroll = p.scroll.min(n.saturating_sub(1));
    if p.selected < scroll {
        scroll = p.selected;
    } else if list_height > 0 && p.selected >= scroll + list_height {
        scroll = p.selected + 1 - list_height;
    }

    if n == 0 {
        let msg = if p.loading { "" } else { "no matching branches" };
        return (
            vec![Row::new(vec![Cell::from(Span::styled(msg, Theme::muted()))])],
            scroll,
        );
    }

    let mut rows: Vec<Row<'static>> = Vec::new();
    for (i, e) in entries.iter().enumerate().skip(scroll).take(list_height) {
        let is_sel = i == p.selected;
        let marker = if is_sel {
            format!("{} ", icons::current_marker())
        } else {
            "  ".to_string()
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        match e {
            CreateEntry::New(name) => {
                let icon = if ascii_mode() { "+" } else { "✨" };
                spans.push(Span::styled(
                    format!("{marker}{icon} new branch \"{name}\""),
                    Theme::accent_bold(),
                ));
            }
            CreateEntry::Branch(b) => {
                spans.push(Span::raw(marker));
                let name_style = if b.checked_out {
                    Theme::muted()
                } else {
                    Style::default().fg(Theme::BRANCH)
                };
                spans.push(Span::styled(b.name.clone(), name_style));
                spans.push(Span::styled(format!("  {}", branch_label(b)), Theme::muted()));
                if b.checked_out {
                    spans.push(Span::styled("  in use", Theme::muted()));
                }
            }
        }
        let row = Row::new(vec![Cell::from(Line::from(spans))]);
        rows.push(if is_sel { row.style(Theme::selected()) } else { row });
    }
    (rows, scroll)
}

/// Draw the centered create-worktree modal over the active view.
fn render_create_picker(f: &mut Frame, app: &mut AppState) {
    let area = centered_rect(60, 70, f.area());
    let inner = bordered_inner(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // filter
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // branch list
            Constraint::Length(1), // hint
        ])
        .split(inner);
    let (filter_area, list_area, hint_area) = (chunks[0], chunks[2], chunks[3]);

    // Phase A: gather everything under an immutable borrow.
    let (title, filter_spans, rows, new_scroll, hint) = {
        let Some(InputMode::Create(p)) = &app.input else {
            return;
        };
        let title = match p.step {
            CreateStep::Branch => " Create worktree ".to_string(),
            CreateStep::Base => {
                format!(" Base branch for \"{}\" ", p.new_branch.as_deref().unwrap_or(""))
            }
        };
        let filter_spans: Vec<Span<'static>> = if p.loading {
            vec![Span::styled("loading branches…", Theme::muted())]
        } else {
            vec![
                Span::styled("filter: ", Theme::muted()),
                Span::raw(p.filter.clone()),
                Span::styled("▏", Style::default().fg(Theme::WORKING)),
            ]
        };
        let (rows, scroll) = picker_rows(p, list_area.height as usize);
        let hint = match p.step {
            CreateStep::Branch => "type to filter · ↑↓ select · enter choose · esc cancel",
            CreateStep::Base => "↑↓ select base · enter create · esc back",
        };
        (title, filter_spans, rows, scroll, hint)
    };

    // Phase B: write back scroll + the clickable list region.
    if let Some(InputMode::Create(p)) = app.input.as_mut() {
        p.scroll = new_scroll;
    }
    app.regions.create_list = Some(list_area);

    // Phase C: render (Clear wipes the panels underneath).
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Theme::ACCENT))
            .title(Span::styled(title, Theme::accent_bold())),
        area,
    );
    f.render_widget(Paragraph::new(Line::from(filter_spans)), filter_area);
    f.render_widget(
        Table::new(rows, [Constraint::Fill(1)]).column_spacing(0),
        list_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Theme::muted()))),
        hint_area,
    );
}

fn git_spans(r: &WorktreeRow) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let unstaged = r.unstaged + r.untracked;
    if unstaged > 0 || r.staged > 0 {
        spans.push(Span::styled(
            format!("{}{}", icons::dirty_marker(), unstaged.max(r.staged)),
            Style::default().fg(Theme::DIRTY).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }
    if r.staged > 0 {
        spans.push(Span::styled(
            format!("+{}", r.staged),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::raw(" "));
    }
    if r.ahead > 0 {
        spans.push(Span::styled(
            format!("↑{}", r.ahead),
            Style::default().fg(Theme::MUTED),
        ));
        spans.push(Span::raw(" "));
    }
    if r.behind > 0 {
        spans.push(Span::styled(
            format!("↓{}", r.behind),
            Style::default().fg(Theme::MUTED),
        ));
        spans.push(Span::raw(" "));
    }
    if r.rebase {
        spans.push(Span::styled(
            "R ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    if r.conflict {
        spans.push(Span::styled(
            "! ",
            Style::default().fg(Theme::DIRTY).add_modifier(Modifier::BOLD),
        ));
    }
    spans
}
