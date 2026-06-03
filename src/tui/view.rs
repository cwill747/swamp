use super::icons;
use super::theme::Theme;
use super::AppState;
use crate::daemon::resources;
use crate::cli::TuiView;
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::github::{CheckState, PrSummary};
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;
use std::time::{Duration, SystemTime};

pub fn render(f: &mut Frame, app: &mut AppState) {
    match app.view {
        TuiView::All => render_all(f, app),
        _ => render_single_panel(f, app),
    }
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
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
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
    let mut spans = Vec::new();
    if app.refreshing {
        let frame = icons::SPINNER_FRAMES[app.spinner_frame % icons::SPINNER_FRAMES.len()];
        spans.push(Span::styled(
            format!("{frame} Refreshing… "),
            Style::default().fg(Theme::WORKING),
        ));
    }
    spans.push(Span::styled(
        "j/k move · enter jump · d delete · r refresh · K kill · q quit",
        Theme::muted(),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_worktree_table(f: &mut Frame, app: &AppState, area: ratatui::layout::Rect) {
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
            Constraint::Length(2),  // PR icon
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
}

fn render_ai_status(f: &mut Frame, app: &AppState, area: ratatui::layout::Rect) {
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

fn render_pr_status(f: &mut Frame, app: &AppState, area: ratatui::layout::Rect) {
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

        let branch_cell = Cell::from(Span::styled(
            truncate(branch, 20),
            Style::default().fg(Theme::BRANCH),
        ));

        let title_cell = Cell::from(Span::styled(truncate(&pr.title, 40), Theme::muted()));

        rows.push(Row::new(vec![
            state_cell,
            number_cell,
            checks_cell,
            branch_cell,
            title_cell,
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // PR state icon
            Constraint::Length(6),  // #number
            Constraint::Length(7),  // checks
            Constraint::Min(10),   // branch
            Constraint::Min(10),   // title
        ],
    )
    .block(block)
    .column_spacing(1);

    f.render_widget(table, area);
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
        let (icon, color) = pr_state_icon_color(pr);
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
    } else {
        row
    }
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
