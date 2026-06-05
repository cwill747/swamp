use super::bordered_inner;
use super::pr::pr_state_icon_color;
use crate::cli::TuiView;
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::github::ReviewDecision;
use crate::tui::AppState;
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use std::time::{Duration, SystemTime};

pub(super) fn render_worktree_table(f: &mut Frame, app: &mut AppState, area: Rect) {
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
            Constraint::Length(3), // # + caret
            Constraint::Length(2), // agent icon
            Constraint::Length(1), // PR icon
            Constraint::Min(8),    // worktree name
            Constraint::Min(10),   // branch
            Constraint::Min(12),   // git
            Constraint::Length(5), // age
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
                Some(ReviewDecision::ChangesRequested) => (icons::review_changes(), Color::Red),
                Some(ReviewDecision::Commented) => (icons::review_commented(), Color::Yellow),
                Some(ReviewDecision::Approved) => (icons::review_approved(), Color::Green),
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

    let row = Row::new(vec![
        idx_cell,
        agent_cell,
        pr_cell,
        name_cell,
        branch_cell,
        git_cell,
        age_cell,
    ]);
    if i == app.selected {
        row.style(Theme::selected())
    } else if is_current {
        row.style(Theme::current())
    } else {
        row
    }
}

pub(super) fn git_spans(r: &WorktreeRow) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let unstaged = r.unstaged + r.untracked;
    if unstaged > 0 || r.staged > 0 {
        spans.push(Span::styled(
            format!("{}{}", icons::dirty_marker(), unstaged.max(r.staged)),
            Style::default()
                .fg(Theme::DIRTY)
                .add_modifier(Modifier::BOLD),
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
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if r.conflict {
        spans.push(Span::styled(
            "! ",
            Style::default()
                .fg(Theme::DIRTY)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}
