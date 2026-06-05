use super::{bordered_inner, truncate};
use crate::github::{CheckState, PrSummary, ReviewDecision};
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::tui::{AppState, PrHit};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Style};
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
                .add_modifier(ratatui::style::Modifier::BOLD),
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
            let (check_icon, check_color) = check_state_icon_color(checks, app.spinner_frame);
            let mut spans = vec![Span::styled(check_icon, Style::default().fg(check_color))];
            match checks {
                CheckState::Failure { passed, total } | CheckState::Pending { passed, total } => {
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

    f.render_widget(table, area);

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
            let frame = icons::SPINNER_FRAMES[spinner_frame % icons::SPINNER_FRAMES.len()];
            (frame.to_string(), Color::Cyan)
        }
    }
}
