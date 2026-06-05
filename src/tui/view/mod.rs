mod ai;
mod picker;
mod pr;
mod resources;
mod worktrees;

use super::icons;
use super::theme::Theme;
use super::{AppState, InputMode};
use crate::cli::TuiView;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

pub use resources::max_resource_scroll;

pub fn render(f: &mut Frame, app: &mut AppState) {
    // Hit regions are rebuilt each frame; panels not drawn this frame stay None.
    app.regions = super::HitRegions::default();
    match app.view {
        TuiView::All => render_all(f, app),
        _ => render_single_panel(f, app),
    }

    // The create picker floats above whatever view is active.
    if matches!(app.input, Some(InputMode::Create(_))) {
        picker::render_create_picker(f, app);
    }
}

/// Inner content area of a fully-bordered block (the row region).
pub(super) fn bordered_inner(area: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(area)
}

fn render_single_panel(f: &mut Frame, app: &mut AppState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    match app.view {
        TuiView::Worktrees => worktrees::render_worktree_table(f, app, chunks[0]),
        TuiView::AiStatus => ai::render_ai_status(f, app, chunks[0]),
        TuiView::Resources => resources::render_resources(f, app, chunks[0]),
        TuiView::PrStatus => pr::render_pr_status(f, app, chunks[0]),
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

    worktrees::render_worktree_table(f, app, left[0]);
    resources::render_resources(f, app, left[1]);
    ai::render_ai_status(f, app, right[0]);
    pr::render_pr_status(f, app, right[1]);

    render_footer(f, app, outer[2]);
}

fn render_footer(f: &mut Frame, app: &AppState, area: Rect) {
    // The delete confirmation takes over the footer. (The create picker is a
    // centered overlay drawn separately in `render`.)
    if let Some(InputMode::ConfirmDelete { name, dirty }) = &app.input {
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
    if let Some((msg, _)) = &app.toast {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(Theme::IDLE_RECENT),
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
    spans.push(Span::styled(footer_hint(app.view), Theme::muted()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Key/mouse hints for the footer. Each single panel advertises only what it
/// actually responds to; the worktree list (and the combined `All` view) is the
/// only place the create/delete/jump keys do anything visible.
fn footer_hint(view: TuiView) -> &'static str {
    match view {
        TuiView::All | TuiView::Worktrees => {
            "j/k move · enter jump · c create · d delete · r refresh · u update · K kill · q quit"
        }
        TuiView::Resources => "j/k scroll · r refresh · q quit",
        TuiView::AiStatus => "dbl-click jump · r refresh · q quit",
        TuiView::PrStatus => "click copies PR url · r refresh · q quit",
    }
}

/// Truncate `s` to at most `max` chars, appending an ellipsis when cut.
pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = max.saturating_sub(1);
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_hint_matches_panel_keys() {
        // Worktree-style panels advertise the full action set.
        assert!(footer_hint(TuiView::Worktrees).contains("enter jump"));
        assert!(footer_hint(TuiView::All).contains("d delete"));

        // Auxiliary panels only advertise keys they actually honor; they must
        // not promise the worktree-only keyboard actions.
        for view in [TuiView::Resources, TuiView::AiStatus, TuiView::PrStatus] {
            let hint = footer_hint(view);
            assert!(hint.contains("q quit"), "{view:?} should advertise quit");
            assert!(
                hint.contains("r refresh"),
                "{view:?} should advertise refresh"
            );
            assert!(
                !hint.contains("enter jump"),
                "{view:?} should not advertise enter-jump"
            );
            assert!(
                !hint.contains("create"),
                "{view:?} should not advertise create"
            );
            assert!(
                !hint.contains("delete"),
                "{view:?} should not advertise delete"
            );
        }

        assert!(footer_hint(TuiView::Resources).contains("j/k scroll"));
    }
}
