use super::worktrees::git_spans;
use super::{bordered_inner, truncate};
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::tui::AppState;
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use std::time::{Duration, SystemTime};

pub(super) fn render_ai_status(f: &mut Frame, app: &mut AppState, area: Rect) {
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
            let name_cell = Cell::from(Span::styled(
                r.name.as_str(),
                Style::default().fg(Color::White),
            ));

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
            Constraint::Length(2), // icon
            Constraint::Length(8), // status label
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
