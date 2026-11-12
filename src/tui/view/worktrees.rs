use super::bordered_inner;
use super::pr::{checks_status_cell, pr_state_icon_color, review_status_cell};
use crate::cli::TuiView;
use crate::config::Harness;
use crate::daemon::state::{AgentStatus, WorktreeRow};
use crate::github::{PrSummary, ReviewDecision};
use crate::tui::AppState;
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::util::{format_compact_age, now_unix, unix_to_systemtime};
use crate::worktree::{SlotState, WorktreeKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};
use std::time::{Duration, SystemTime};

const EXPANDED_PR_COLUMNS_WIDTH: u16 = 100;
/// Below this inner width the slot label joins the title cell, and the title
/// and branch columns get minimum widths ahead of the status columns.
const NARROW_WIDTH: u16 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorktreeTableLayout {
    Narrow,
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

    // The default dashboard gives this pane a third of the terminal, so it is
    // often narrow. Min constraints win over Length ones, so the title and
    // branch keep room and the git and age columns shrink first.
    let narrow_constraints = [
        Constraint::Length(3),  // # + caret
        Constraint::Length(2),  // agent icon
        Constraint::Length(1),  // PR icon
        Constraint::Min(12),    // slot label + work title
        Constraint::Min(8),     // branch
        Constraint::Length(12), // git
        Constraint::Length(5),  // age
        Constraint::Length(1),  // harness override (C/X)
    ];
    let compact_constraints = [
        Constraint::Length(3),  // # + caret
        Constraint::Length(2),  // agent icon
        Constraint::Length(1),  // PR icon
        Constraint::Length(6),  // slot label (work<N>)
        Constraint::Fill(3),    // work title: most of the spare width
        Constraint::Fill(2),    // branch
        Constraint::Length(12), // git
        Constraint::Length(5),  // age
        Constraint::Length(1),  // harness override (C/X)
    ];
    let expanded_constraints = [
        Constraint::Length(3),  // # + caret
        Constraint::Length(2),  // agent icon
        Constraint::Length(2),  // PR state icon
        Constraint::Length(7),  // #number
        Constraint::Length(7),  // checks
        Constraint::Length(2),  // review
        Constraint::Length(6),  // slot label (work<N>)
        Constraint::Fill(3),    // work title: most of the spare width
        Constraint::Fill(2),    // branch
        Constraint::Length(10), // git
        Constraint::Length(5),  // age
        Constraint::Length(1),  // harness override (C/X)
    ];
    let table = Table::new(
        rows,
        match ctx.layout {
            WorktreeTableLayout::Narrow => narrow_constraints.as_slice(),
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
    match bordered_inner(area).width {
        w if w >= EXPANDED_PR_COLUMNS_WIDTH => WorktreeTableLayout::Expanded,
        w if w >= NARROW_WIDTH => WorktreeTableLayout::Compact,
        _ => WorktreeTableLayout::Narrow,
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

    let agent_cell = if r.is_default {
        // The trunk row shows a star in the agent-icon slot instead of an agent
        // status — agent activity on `main` is not a tracked workflow.
        Cell::from(Span::styled(
            icons::default_branch(),
            Style::default().fg(Theme::DEFAULT_BRANCH),
        ))
    } else {
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

    let mut name_style = if is_pinned {
        Style::default()
            .fg(Theme::ACCENT)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else if is_current {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    // The default-branch row uses its own accent for name and branch, keeping
    // any current/pinned emphasis (bold/underline) but overriding the color.
    if r.is_default {
        name_style = name_style.fg(Theme::DEFAULT_BRANCH);
    }
    // A removal in flight overrides every other color: the row reads as
    // "going away" regardless of whether it's pinned, current, or default.
    if r.deleting {
        name_style = name_style.fg(Theme::MUTED);
    }
    // A free or pinned slot holds no work: mute its title and label.
    let idle_slot = matches!(r.slot_state, Some(SlotState::Free | SlotState::Pinned));
    if idle_slot {
        name_style = name_style.fg(Theme::MUTED);
    }
    let label = row_label(r);
    let narrow = ctx.layout == WorktreeTableLayout::Narrow;
    let name_cell = if narrow && !label.is_empty() {
        Cell::from(Line::from(vec![
            Span::styled(format!("{label} "), Theme::muted()),
            Span::styled(row_title(r), name_style),
        ]))
    } else {
        Cell::from(Span::styled(row_title(r), name_style))
    };
    let label_cell = Cell::from(Span::styled(label, Theme::muted()));

    let branch_color = if r.deleting {
        Theme::MUTED
    } else if r.is_default {
        Theme::DEFAULT_BRANCH
    } else {
        Theme::BRANCH
    };
    // A free slot's `detached@<sha>` label is noise: it holds no branch.
    let branch_text = if idle_slot { "" } else { r.branch.as_str() };
    let branch_cell = Cell::from(Span::styled(branch_text, Style::default().fg(branch_color)));

    // A row being deleted replaces its git-status column with a spinner and
    // "deleting…", shared across every subscribed TUI via `WorktreeRow::deleting`
    // — not just the pane that pressed `d`.
    let git_cell = if r.deleting {
        let frame = icons::SPINNER_FRAMES[app.spinner_frame % icons::SPINNER_FRAMES.len()];
        Cell::from(Span::styled(
            format!("{frame} deleting…"),
            Style::default()
                .fg(Theme::DIRTY)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Cell::from(Line::from(git_spans(r)))
    };

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

    let mut cells = vec![idx_cell, agent_cell];

    if ctx.layout == WorktreeTableLayout::Expanded {
        if r.is_default {
            // The trunk row never shows PR/CI/review/comment status, and never a
            // PR-loading indicator — push blank cells for every PR column,
            // regardless of `pr_snapshot.loading`.
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
        } else if let Some(pr) = app.pr_snapshot.prs.get(&r.branch) {
            cells.extend(pr_status_cells(pr, app.spinner_frame));
        } else if app.pr_snapshot.loading {
            // PR data hasn't landed yet: mark the row as loading in the PR state
            // column rather than leaving it blank (which reads as "no PR"). A
            // static muted glyph, never the cyan CI build-running spinner.
            cells.push(Cell::from(Span::styled(
                icons::pr_loading(),
                Theme::muted(),
            )));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
        } else {
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
            cells.push(Cell::from(""));
        }
    } else if r.is_default {
        // Compact layout: blank PR cell for the trunk row, no loading glyph.
        cells.push(Cell::from(Span::raw(" ")));
    } else {
        cells.push(compact_pr_cell(
            app.pr_snapshot.prs.get(&r.branch),
            app.pr_snapshot.loading,
        ));
    }

    if !narrow {
        cells.push(label_cell);
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

/// The row's work title, or its worktree name when the daemon sent no title
/// (an older daemon).
fn row_title(r: &WorktreeRow) -> &str {
    if r.title.is_empty() {
        &r.name
    } else {
        &r.title
    }
}

/// The short label column: the slot name for a slot, blank otherwise (an
/// overflow worktree's name repeats its branch).
fn row_label(r: &WorktreeRow) -> &str {
    if r.kind == WorktreeKind::Slot {
        &r.name
    } else {
        ""
    }
}

fn compact_pr_cell<'a>(pr: Option<&PrSummary>, loading: bool) -> Cell<'a> {
    if let Some(pr) = pr {
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
    } else if loading {
        // No summary yet but a fetch is outstanding: show a muted loading glyph
        // (static, distinct from the CI spinner) instead of a blank "no PR" cell.
        Cell::from(Span::styled(icons::pr_loading(), Theme::muted()))
    } else {
        Cell::from(Span::raw(" "))
    }
}

fn pr_status_cells<'a>(pr: &PrSummary, spinner_frame: usize) -> [Cell<'a>; 4] {
    let (icon, color) = pr_state_icon_color(pr);
    let state_cell = Cell::from(Span::styled(icon, Style::default().fg(color)));
    let number_cell = Cell::from(Span::styled(
        format!("#{}", pr.number),
        Style::default().fg(color),
    ));
    let checks_cell = checks_status_cell(&pr.checks, spinner_frame);

    [
        state_cell,
        number_cell,
        checks_cell,
        review_status_cell(&pr.review),
    ]
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
    use crate::github::CheckState;

    fn pr_with_checks(checks: Option<CheckState>) -> PrSummary {
        PrSummary {
            number: 1,
            title: "PR".into(),
            state: "OPEN".into(),
            head_oid: None,
            is_draft: false,
            checks,
            check_meta: None,
            url: None,
            comment_count: 0,
            review: None,
            reviews_partial: false,
        }
    }

    fn row(name: &str, kind: WorktreeKind, title: &str) -> WorktreeRow {
        let json = serde_json::json!({
            "name": name, "path": format!("/r/{name}"), "branch": "b", "upstream": null,
            "ahead": 0, "behind": 0, "staged": 0, "unstaged": 0, "untracked": 0,
            "conflict": false, "rebase": false, "agent": "idle", "agent_ts": 0,
            "kind": kind, "title": title,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn title_and_label_cells() {
        let slot = row("work3", WorktreeKind::Slot, "Fix flaky CI retry logic");
        assert_eq!(row_title(&slot), "Fix flaky CI retry logic");
        assert_eq!(row_label(&slot), "work3");

        let overflow = row("spike-x", WorktreeKind::Overflow, "Spike");
        assert_eq!(row_label(&overflow), "");

        // An older daemon sends no title: fall back to the worktree name.
        let old = row("feature", WorktreeKind::Overflow, "");
        assert_eq!(row_title(&old), "feature");
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
    fn pr_status_cells_match_pr_status_columns() {
        let cells = pr_status_cells(
            &pr_with_checks(Some(CheckState::Failure {
                passed: 3,
                total: 5,
                failed: 1,
            })),
            0,
        );

        assert_eq!(cells.len(), 4);
    }
}
