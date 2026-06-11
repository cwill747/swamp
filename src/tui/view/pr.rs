use super::{bordered_inner, truncate};
use crate::github::{CheckState, PrSummary, ReviewDecision};
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::tui::{AppState, PrHit};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

pub(super) fn render_pr_status(f: &mut Frame, app: &mut AppState, area: Rect) {
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

    // If there is a fetch error, reserve one line at the bottom of the block
    // for a dim status line.  We do this regardless of whether there are PRs
    // to show, so stale data and failures are always surfaced.
    let has_error = app.pr_snapshot.error.is_some();

    // Split the area only when we actually have an error line to render.
    let (table_area, error_area) = if has_error && area.height >= 4 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };

    let mut pr_rows: Vec<(&str, &PrSummary)> = Vec::new();
    for row in &app.snapshot.rows {
        if let Some(pr) = app.pr_snapshot.prs.get(&row.branch) {
            pr_rows.push((&row.branch, pr));
        }
    }

    if pr_rows.is_empty() {
        let empty_msg = if has_error && app.pr_snapshot.fetched_at.is_none() {
            "github unreachable"
        } else {
            "No PRs for any worktree branch"
        };
        let lines = vec![Line::from(Span::styled(empty_msg, Theme::muted()))];
        f.render_widget(Paragraph::new(lines).block(block), table_area);
        // Render error line if present.
        if let (Some(err_area), Some(err)) = (error_area, &app.pr_snapshot.error) {
            render_error_line(f, err_area, err, app.pr_snapshot.fetched_at);
        }
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

        let checks_cell = checks_status_cell(&pr.checks, app.spinner_frame);

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
            Constraint::Length(2), // PR state icon
            Constraint::Length(7), // #number
            Constraint::Length(7), // checks
            Constraint::Length(2), // review
            Constraint::Min(10),   // branch
            Constraint::Fill(1),   // title
        ],
    )
    .block(block)
    .column_spacing(1);

    f.render_widget(table, table_area);

    // Render the error/staleness line below the table (inside the outer area).
    if let (Some(err_area), Some(err)) = (error_area, &app.pr_snapshot.error) {
        render_error_line(f, err_area, err, app.pr_snapshot.fetched_at);
    }

    // One PrHit per visible row, in the same order as `rows` above.
    let hits: Vec<PrHit> = pr_rows
        .iter()
        .take(max_rows)
        .map(|(_, pr)| PrHit {
            url: pr.url.clone(),
        })
        .collect();
    drop(pr_rows);
    app.regions.prs = Some((inner, hits));
}

/// Render a single-line dim error/staleness notice.
fn render_error_line(f: &mut Frame, area: Rect, err: &str, fetched_at: Option<u64>) {
    // Truncate the error message so it fits on one line.
    let short_err = truncate(err, 40);

    let text = if let Some(ts) = fetched_at {
        let age_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_sub(ts);
        let age = format_age(age_secs);
        format!(" github: {short_err} — data as of {age} ago")
    } else {
        format!(" github: {short_err}")
    };

    let line = Line::from(Span::styled(
        text,
        Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
    ));
    f.render_widget(Paragraph::new(line), area);
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

pub(super) fn pr_state_icon_color(pr: &PrSummary) -> (&'static str, Color) {
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

pub(super) fn review_status_cell<'a>(review: &Option<ReviewDecision>) -> Cell<'a> {
    review_status_span(review).map_or_else(|| Cell::from(""), Cell::from)
}

fn review_status_span(review: &Option<ReviewDecision>) -> Option<Span<'static>> {
    match review {
        Some(ReviewDecision::Approved) => Some(Span::styled(
            icons::review_approved(),
            Style::default().fg(Color::Green),
        )),
        Some(ReviewDecision::ChangesRequested) => Some(Span::styled(
            icons::review_changes(),
            Style::default().fg(Color::Red),
        )),
        Some(ReviewDecision::Commented) => Some(Span::styled(
            icons::review_commented(),
            Style::default().fg(Color::Yellow),
        )),
        Some(ReviewDecision::ReviewRequired) => Some(Span::styled("?", Theme::muted())),
        _ => None,
    }
}

pub(super) fn checks_status_cell<'a>(
    checks: &Option<CheckState>,
    spinner_frame: usize,
) -> Cell<'a> {
    if let Some(checks) = checks {
        let (check_icon, check_color) = check_state_icon_color(checks, spinner_frame);
        let mut spans = vec![Span::styled(check_icon, Style::default().fg(check_color))];
        match checks {
            CheckState::Failure { passed, total, .. } | CheckState::Pending { passed, total } => {
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
    }
}

fn check_state_icon_color(checks: &CheckState, spinner_frame: usize) -> (String, Color) {
    match checks {
        CheckState::Success => (icons::check_success().to_string(), Color::Green),
        CheckState::Failure { .. } => (icons::check_failure().to_string(), Color::Red),
        CheckState::Pending { .. } => {
            let frame = icons::SPINNER_FRAMES[spinner_frame % icons::SPINNER_FRAMES.len()];
            (frame.to_string(), Color::Cyan)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_status_cell_marks_required_reviews() {
        let span = review_status_span(&Some(ReviewDecision::ReviewRequired)).unwrap();
        assert_eq!(span.content, "?");
    }
}
