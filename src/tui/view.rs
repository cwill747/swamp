use super::icons;
use super::theme::Theme;
use super::AppState;
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
        TuiView::Worktrees => render_worktree_table(f, app, f.area()),
        TuiView::AiStatus => render_ai_status(f, app, f.area()),
        TuiView::Resources => render_resources(f, f.area()),
        TuiView::PrStatus => render_pr_status(f, f.area()),
    }
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
    render_resources(f, left[1]);
    render_ai_status(f, app, right[0]);
    render_pr_status(f, right[1]);

    // Footer.
    let mut footer_spans = Vec::new();
    if app.refreshing {
        let frame = icons::SPINNER_FRAMES[app.spinner_frame % icons::SPINNER_FRAMES.len()];
        footer_spans.push(Span::styled(
            format!("{frame} Refreshing… "),
            Style::default().fg(Theme::WORKING),
        ));
    }
    footer_spans.push(Span::styled(
        "j/k move · enter jump · r refresh · K kill · q quit",
        Theme::muted(),
    ));
    f.render_widget(Paragraph::new(Line::from(footer_spans)), outer[2]);
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
    .block({
        let mut title_spans = vec![Span::styled(" Worktrees ", Theme::accent_bold())];
        if app.refreshing {
            let frame = icons::SPINNER_FRAMES[app.spinner_frame % icons::SPINNER_FRAMES.len()];
            title_spans.push(Span::styled(
                format!("{frame} Refreshing… "),
                Style::default().fg(Theme::WORKING),
            ));
        }
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Theme::MUTED))
            .title(Line::from(title_spans))
    })
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

fn render_resources(f: &mut Frame, area: ratatui::layout::Rect) {
    let lines = vec![
        Line::from(Span::styled("No resource data", Theme::muted())),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(Span::styled(" Resources ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)));

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
