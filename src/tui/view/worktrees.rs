use super::bordered_inner;
use super::pr::pr_state_icon_color;
use crate::cli::TuiView;
use crate::config::Harness;
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::github::{CheckState, PrSummary, ReviewDecision};
use crate::tui::AppState;
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};
use std::time::{Duration, SystemTime};

const EXPANDED_PR_COLUMNS_WIDTH: u16 = 92;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorktreeTableLayout {
    Compact,
    Expanded,
}

struct WorktreeRowContext<'a> {
    selected: Option<usize>,
    now: u64,
    current_tab: Option<&'a str>,
    pin_current: bool,
    layout: WorktreeTableLayout,
}

pub(super) fn render_worktree_table(f: &mut Frame, app: &mut AppState, area: Rect) {
    let now = now_unix();
    let ctx = WorktreeRowContext {
        selected: app.selected_index(),
        current_tab: app.current_tab.as_deref(),
        pin_current: app.view == TuiView::Worktrees,
        layout: worktree_table_layout(area),
        now,
    };

    let rows: Vec<Row> = app
        .snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| build_row(i, r, app, &ctx))
        .collect();

    let compact_constraints = [
        Constraint::Length(3), // # + caret
        Constraint::Length(2), // agent icon
        Constraint::Length(1), // PR icon
        Constraint::Min(8),    // worktree name
        Constraint::Min(10),   // branch
        Constraint::Min(12),   // git
        Constraint::Length(5), // age
        Constraint::Length(1), // harness override (C/X)
    ];
    let expanded_constraints = [
        Constraint::Length(3), // # + caret
        Constraint::Length(2), // agent icon
        Constraint::Length(1), // PR icon
        Constraint::Length(3), // failed builds
        Constraint::Length(4), // comments
        Constraint::Length(2), // review
        Constraint::Min(8),    // worktree name
        Constraint::Min(10),   // branch
        Constraint::Min(10),   // git
        Constraint::Length(5), // age
        Constraint::Length(1), // harness override (C/X)
    ];
    let table = Table::new(
        rows,
        match ctx.layout {
            WorktreeTableLayout::Compact => compact_constraints.as_slice(),
            WorktreeTableLayout::Expanded => expanded_constraints.as_slice(),
        },
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Theme::MUTED))
            .title(Span::styled(" Worktrees ", Theme::accent_bold())),
    )
    .column_spacing(1);

    let inner = bordered_inner(area);
    let visible_capacity = inner.height as usize;
    let selected = ctx.selected;
    if let Some(idx) = selected {
        if idx < app.worktree_scroll {
            app.worktree_scroll = idx;
        } else if visible_capacity > 0 && idx >= app.worktree_scroll + visible_capacity {
            app.worktree_scroll = idx + 1 - visible_capacity;
        }
    }
    if app.snapshot.rows.len() <= visible_capacity {
        app.worktree_scroll = 0;
    } else {
        app.worktree_scroll = app
            .worktree_scroll
            .min(app.snapshot.rows.len().saturating_sub(visible_capacity));
    }
    let mut state = TableState::new()
        .with_selected(selected)
        .with_offset(app.worktree_scroll);
    f.render_stateful_widget(table, area, &mut state);

    let visible = app
        .snapshot
        .rows
        .len()
        .saturating_sub(app.worktree_scroll)
        .min(visible_capacity);
    app.regions.worktrees = Some((inner, visible, app.worktree_scroll));
}

fn worktree_table_layout(area: Rect) -> WorktreeTableLayout {
    if bordered_inner(area).width >= EXPANDED_PR_COLUMNS_WIDTH {
        WorktreeTableLayout::Expanded
    } else {
        WorktreeTableLayout::Compact
    }
}

fn build_row<'a>(
    i: usize,
    r: &'a WorktreeRow,
    app: &AppState,
    ctx: &WorktreeRowContext<'_>,
) -> Row<'a> {
    let is_current = ctx.current_tab.map(|t| t == r.name).unwrap_or(false);
    let is_pinned = ctx.pin_current && is_current;
    let recent = ctx.now.saturating_sub(r.agent_ts) < 300;

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

    // Per-worktree harness override (set with `h`); only honored when the repo
    // setting is `choose`, but shown whenever recorded. Blank if unset.
    let harness_cell = match r.harness {
        Some(Harness::Claude) => Cell::from(Span::styled("C", Style::default().fg(Theme::BRANCH))),
        Some(Harness::Codex) => Cell::from(Span::styled("X", Style::default().fg(Theme::BRANCH))),
        None => Cell::from(Span::raw(" ")),
    };

    let mut cells = vec![idx_cell, agent_cell, pr_cell];

    if ctx.layout == WorktreeTableLayout::Expanded {
        if let Some(pr) = app.pr_snapshot.prs.get(&r.branch) {
            cells.push(failed_builds_cell(pr));
            cells.push(comment_count_cell(pr));
            cells.push(review_status_cell(&pr.review));
        } else {
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
        }
    }

    cells.extend([name_cell, branch_cell, git_cell, age_cell, harness_cell]);
    let row = Row::new(cells);
    if ctx.selected == Some(i) {
        row.style(Theme::selected())
    } else if is_current {
        row.style(Theme::current())
    } else {
        row
    }
}

fn failed_builds_cell<'a>(pr: &PrSummary) -> Cell<'a> {
    Cell::from(match failed_build_count(pr) {
        Some(count) => Span::styled(count.to_string(), Style::default().fg(Color::Red)),
        None => Span::raw(""),
    })
}

fn comment_count_cell<'a>(pr: &PrSummary) -> Cell<'a> {
    if pr.comment_count > 0 {
        Cell::from(Span::styled(
            pr.comment_count.to_string(),
            Style::default().fg(Color::Yellow),
        ))
    } else {
        Cell::from("")
    }
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
        Some(ReviewDecision::ReviewRequired) => Cell::from(Span::styled("?", Theme::muted())),
        None => Cell::from(""),
    }
}

fn failed_build_count(pr: &PrSummary) -> Option<u32> {
    match pr.checks {
        Some(CheckState::Failure {
            passed,
            total,
            failed,
        }) => Some(if failed > 0 {
            failed
        } else {
            total.saturating_sub(passed).max(1)
        }),
        _ => None,
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
    if r.upstream_gone {
        spans.push(Span::styled(
            "gone ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pr_with_checks(checks: Option<CheckState>) -> PrSummary {
        PrSummary {
            number: 1,
            title: "PR".into(),
            state: "OPEN".into(),
            is_draft: false,
            checks,
            check_meta: None,
            url: None,
            comment_count: 0,
            review: None,
            reviews_partial: false,
        }
    }

    #[test]
    fn worktree_table_layout_expands_only_when_inner_width_fits() {
        assert_eq!(
            worktree_table_layout(Rect::new(0, 0, EXPANDED_PR_COLUMNS_WIDTH + 2, 8)),
            WorktreeTableLayout::Expanded
        );
        assert_eq!(
            worktree_table_layout(Rect::new(0, 0, EXPANDED_PR_COLUMNS_WIDTH + 1, 8)),
            WorktreeTableLayout::Compact
        );
    }

    #[test]
    fn failed_build_count_only_reports_failures() {
        assert_eq!(
            failed_build_count(&pr_with_checks(Some(CheckState::Failure {
                passed: 3,
                total: 5,
                failed: 2
            }))),
            Some(2)
        );
        assert_eq!(
            failed_build_count(&pr_with_checks(Some(CheckState::Failure {
                passed: 3,
                total: 5,
                failed: 1
            }))),
            Some(1)
        );
        assert_eq!(
            failed_build_count(&pr_with_checks(Some(CheckState::Pending {
                passed: 3,
                total: 5
            }))),
            None
        );
        assert_eq!(failed_build_count(&pr_with_checks(None)), None);
    }
}
