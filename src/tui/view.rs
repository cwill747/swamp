use super::icons;
use super::theme::Theme;
use super::AppState;
use crate::daemon::resources;
use crate::cli::TuiView;
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;
use std::time::{Duration, SystemTime};

pub fn render(f: &mut Frame, app: &AppState) {
    match app.view {
        TuiView::All => render_all(f, app),
        _ => render_single_panel(f, app),
    }
}

fn render_single_panel(f: &mut Frame, app: &AppState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    match app.view {
        TuiView::Worktrees => render_worktree_table(f, app, chunks[0]),
        TuiView::AiStatus => render_ai_status(f, app, chunks[0]),
        TuiView::Resources => render_resources(f, app, chunks[0]),
        TuiView::PrStatus => render_pr_status(f, chunks[0]),
        TuiView::All => unreachable!(),
    }

    render_footer(f, app, chunks[1]);
}

fn render_all(f: &mut Frame, app: &AppState) {
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
    render_pr_status(f, right[1]);

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
    let current_tab = std::env::var("ZELLIJ_TAB_NAME").ok();
    let rows: Vec<Row> = app
        .snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| build_row(i, r, app, now, current_tab.as_deref()))
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),  // # + caret
            Constraint::Length(2),  // agent icon
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
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Recent Sessions (Claude)",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    let now = now_unix();
    let mut any_active = false;
    for r in &app.snapshot.rows {
        if r.agent == AgentStatus::Working || r.agent == AgentStatus::Waiting {
            any_active = true;
            let status_span = match r.agent {
                AgentStatus::Working => Span::styled(
                    " working ",
                    Style::default().fg(Theme::WORKING).add_modifier(Modifier::BOLD),
                ),
                AgentStatus::Waiting => Span::styled(
                    " waiting ",
                    Style::default().fg(Theme::WAITING).add_modifier(Modifier::BOLD),
                ),
                _ => Span::raw(""),
            };
            let age = if r.agent_ts > 0 {
                let dur = SystemTime::now()
                    .duration_since(unix_to_systemtime(r.agent_ts))
                    .unwrap_or(Duration::ZERO);
                format_compact_age(dur)
            } else {
                "-".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(&r.name, Style::default().fg(Color::White)),
                Span::raw("  "),
                status_span,
                Span::raw("  "),
                Span::styled(age, Theme::muted()),
            ]));
        }
    }

    let recent = now.saturating_sub(300);
    for r in &app.snapshot.rows {
        if r.agent == AgentStatus::Idle && r.agent_ts >= recent {
            any_active = true;
            let age = {
                let dur = SystemTime::now()
                    .duration_since(unix_to_systemtime(r.agent_ts))
                    .unwrap_or(Duration::ZERO);
                format_compact_age(dur)
            };
            lines.push(Line::from(vec![
                Span::styled(&r.name, Style::default().fg(Color::White)),
                Span::raw("  "),
                Span::styled(
                    " done ",
                    Style::default().fg(Theme::IDLE_RECENT),
                ),
                Span::raw("  "),
                Span::styled(age, Theme::muted()),
            ]));
        }
    }

    if !any_active {
        lines.push(Line::from(Span::styled(
            "No active sessions",
            Theme::muted(),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(Span::styled(" AI Status ", Style::default().fg(Theme::WORKING).add_modifier(Modifier::BOLD)));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_resources(f: &mut Frame, app: &AppState, area: ratatui::layout::Rect) {
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

    // Per-process rows (top N by CPU; cap to area height).
    let max_rows = area.height.saturating_sub(2 /* borders */ + 4 /* header lines */) as usize;
    for p in res.procs.iter().take(max_rows) {
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

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_pr_status(f: &mut Frame, area: ratatui::layout::Rect) {
    let lines = vec![
        Line::from(Span::styled("No open PRs for any worktree branch", Theme::muted())),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(Span::styled(" PR & CI Status ", Style::default().fg(Theme::BRANCH).add_modifier(Modifier::BOLD)));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn build_row<'a>(
    i: usize,
    r: &'a WorktreeRow,
    app: &AppState,
    now: u64,
    current_tab: Option<&str>,
) -> Row<'a> {
    let is_current = current_tab.map(|t| t == r.name).unwrap_or(false);
    let recent = now.saturating_sub(r.agent_ts) < 300;

    let idx_cell = {
        let mut spans = Vec::new();
        if is_current {
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

    let name_style = if is_current {
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

    let row = Row::new(vec![idx_cell, agent_cell, name_cell, branch_cell, git_cell, age_cell]);
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
