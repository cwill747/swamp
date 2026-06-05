use super::bordered_inner;
use crate::tui::icons;
use crate::tui::theme::Theme;
use crate::tui::{AppState, CreateEntry, CreatePicker, CreateStep, InputMode};
use crate::util::ascii_mode;
use crate::worktree::{BranchInfo, BranchKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table};

/// A centered rect sized `pct_w`% × `pct_h`% of `area`.
fn centered_rect(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let w = (area.width * pct_w / 100).clamp(1, area.width);
    let h = (area.height * pct_h / 100).clamp(1, area.height);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// The kind/remote label shown next to a branch row, e.g. `(local, default)`.
fn branch_label(b: &BranchInfo) -> String {
    match (&b.kind, &b.remote) {
        (BranchKind::Remote, Some(r)) => format!("({r})"),
        (BranchKind::Remote, None) => "(remote)".to_string(),
        (BranchKind::Local, _) if b.is_default => "(local, default)".to_string(),
        (BranchKind::Local, _) => "(local)".to_string(),
    }
}

/// Build the visible list rows for the picker, adjusting `scroll` to keep the
/// selection in view. Returns `(rows, new_scroll)`.
fn picker_rows(p: &CreatePicker, list_height: usize) -> (Vec<Row<'static>>, usize) {
    let entries = p.entries();
    let n = entries.len();

    // Keep the selected entry within the visible window.
    let mut scroll = p.scroll.min(n.saturating_sub(1));
    if p.selected < scroll {
        scroll = p.selected;
    } else if list_height > 0 && p.selected >= scroll + list_height {
        scroll = p.selected + 1 - list_height;
    }

    if n == 0 {
        let msg = if p.loading {
            ""
        } else {
            "no matching branches"
        };
        return (
            vec![Row::new(vec![Cell::from(Span::styled(
                msg,
                Theme::muted(),
            ))])],
            scroll,
        );
    }

    let mut rows: Vec<Row<'static>> = Vec::new();
    for (i, e) in entries.iter().enumerate().skip(scroll).take(list_height) {
        let is_sel = i == p.selected;
        let marker = if is_sel {
            format!("{} ", icons::current_marker())
        } else {
            "  ".to_string()
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        match e {
            CreateEntry::New(name) => {
                let icon = if ascii_mode() { "+" } else { "✨" };
                spans.push(Span::styled(
                    format!("{marker}{icon} new branch \"{name}\""),
                    Theme::accent_bold(),
                ));
            }
            CreateEntry::Branch(b) => {
                spans.push(Span::raw(marker));
                let name_style = if b.checked_out {
                    Theme::muted()
                } else {
                    Style::default().fg(Theme::BRANCH)
                };
                spans.push(Span::styled(b.name.clone(), name_style));
                spans.push(Span::styled(
                    format!("  {}", branch_label(b)),
                    Theme::muted(),
                ));
                if b.checked_out {
                    spans.push(Span::styled("  in use", Theme::muted()));
                }
            }
        }
        let row = Row::new(vec![Cell::from(Line::from(spans))]);
        rows.push(if is_sel {
            row.style(Theme::selected())
        } else {
            row
        });
    }
    (rows, scroll)
}

/// Draw the centered create-worktree modal over the active view.
pub(super) fn render_create_picker(f: &mut Frame, app: &mut AppState) {
    let area = centered_rect(60, 70, f.area());
    let inner = bordered_inner(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // filter
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // branch list
            Constraint::Length(1), // hint
        ])
        .split(inner);
    let (filter_area, list_area, hint_area) = (chunks[0], chunks[2], chunks[3]);

    // Phase A: gather everything under an immutable borrow.
    let (title, filter_spans, rows, new_scroll, hint) = {
        let Some(InputMode::Create(p)) = &app.input else {
            return;
        };
        let title = match p.step {
            CreateStep::Branch => " Create worktree ".to_string(),
            CreateStep::Base => {
                format!(
                    " Base branch for \"{}\" ",
                    p.new_branch.as_deref().unwrap_or("")
                )
            }
        };
        let filter_spans: Vec<Span<'static>> = if p.loading {
            vec![Span::styled("loading branches…", Theme::muted())]
        } else {
            vec![
                Span::styled("filter: ", Theme::muted()),
                Span::raw(p.filter.clone()),
                Span::styled("▏", Style::default().fg(Theme::WORKING)),
            ]
        };
        let (rows, scroll) = picker_rows(p, list_area.height as usize);
        let hint = match p.step {
            CreateStep::Branch => "type to filter · ↑↓ select · enter choose · esc cancel",
            CreateStep::Base => "↑↓ select base · enter create · esc back",
        };
        (title, filter_spans, rows, scroll, hint)
    };

    // Phase B: write back scroll + the clickable list region.
    if let Some(InputMode::Create(p)) = app.input.as_mut() {
        p.scroll = new_scroll;
    }
    app.regions.create_list = Some(list_area);

    // Phase C: render (Clear wipes the panels underneath).
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Theme::ACCENT))
            .title(Span::styled(title, Theme::accent_bold())),
        area,
    );
    f.render_widget(Paragraph::new(Line::from(filter_spans)), filter_area);
    f.render_widget(
        Table::new(rows, [Constraint::Fill(1)]).column_spacing(0),
        list_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Theme::muted()))),
        hint_area,
    );
}
