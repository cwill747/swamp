//! Render the cross-repo overview.

use super::app::App;
use super::attention::Reason;
use crate::util::{ascii_mode, format_compact_age};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::Duration;

const FOOTER_HINT: &str = "j/k move · enter jump or launch · U update all · r refresh · q quit";
const OLD_DAEMON_HINT: &str =
    "Some sessions run an older swamp that cannot be identified. Restart them to enable jump.";
const MAX_REPO_WIDTH: usize = 24;
const MAX_WORKTREE_WIDTH: usize = 32;

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn heading() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn reason_color(reason: Reason) -> Color {
    match reason {
        Reason::Waiting => Color::Yellow,
        Reason::ChecksFailed | Reason::Conflict | Reason::Rebase => Color::Red,
        Reason::ChangesRequested => Color::Magenta,
        Reason::IdleDirty => Color::DarkGray,
    }
}

/// Cut `s` to `max` characters, with an ellipsis when it is cut.
fn fit(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push(if ascii_mode() { '~' } else { '…' });
    out
}

fn column_width<'a>(values: impl Iterator<Item = &'a str>, max: usize) -> usize {
    values
        .map(|v| v.chars().count())
        .max()
        .unwrap_or(0)
        .min(max)
}

fn row_line(selected: bool, spans: Vec<Span<'static>>) -> Line<'static> {
    let marker = match (selected, ascii_mode()) {
        (false, _) => "  ",
        (true, true) => "> ",
        (true, false) => "▸ ",
    };
    let mut all = vec![Span::raw(marker)];
    all.extend(spans);
    let line = Line::from(all);
    if selected {
        line.style(
            Style::default()
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        line
    }
}

/// The body lines, and the index of the line that holds the selection.
fn body(app: &App, now: u64) -> (Vec<Line<'static>>, usize) {
    let mut lines = Vec::new();
    let mut selected_line = 0;

    if app.items.is_empty() {
        let text = if app.live_count() == 0 {
            "No swamp session runs."
        } else {
            "Nothing needs attention."
        };
        lines.push(Line::from(Span::styled(text, muted())));
    } else {
        lines.push(Line::from(Span::styled("Needs attention", heading())));
        let repo_width = column_width(app.items.iter().map(|i| i.repo.as_str()), MAX_REPO_WIDTH);
        let worktree_width = column_width(
            app.items.iter().map(|i| i.worktree.as_str()),
            MAX_WORKTREE_WIDTH,
        );
        for (index, item) in app.items.iter().enumerate() {
            let selected = index == app.selected;
            if selected {
                selected_line = lines.len();
            }
            let mut spans = vec![
                Span::styled(
                    format!("{:<repo_width$}  ", fit(&item.repo, MAX_REPO_WIDTH)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!(
                    "{:<worktree_width$}  ",
                    fit(&item.worktree, MAX_WORKTREE_WIDTH)
                )),
                Span::styled(
                    item.reason.label(),
                    Style::default().fg(reason_color(item.reason)),
                ),
            ];
            if let Some(since) = item.since {
                let age = Duration::from_secs(now.saturating_sub(since));
                spans.push(Span::styled(
                    format!("  {}", format_compact_age(age)),
                    muted(),
                ));
            }
            if !app.can_jump(item) {
                spans.push(Span::styled("  (no jump)", muted()));
            }
            lines.push(row_line(selected, spans));
        }
    }

    if !app.cold.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Not running", heading())));
        let name_width = column_width(app.cold.iter().map(|c| c.name.as_str()), MAX_REPO_WIDTH);
        for (index, cold) in app.cold.iter().enumerate() {
            let selected = app.items.len() + index == app.selected;
            if selected {
                selected_line = lines.len();
            }
            let container = cold
                .entry
                .common_dir
                .parent()
                .unwrap_or(&cold.entry.common_dir);
            lines.push(row_line(
                selected,
                vec![
                    Span::styled(
                        format!("{:<name_width$}  ", fit(&cold.name, MAX_REPO_WIDTH)),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(container.display().to_string(), muted()),
                ],
            ));
        }
    }

    if app.unidentified_count() > 0 {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            OLD_DAEMON_HINT,
            Style::default().fg(Color::Yellow),
        )));
    }
    (lines, selected_line)
}

/// Draw the overview. `now` is the current Unix time, for the item ages.
pub fn render(f: &mut Frame, app: &App, now: u64) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let header = Line::from(vec![
        Span::styled("swamp overview", heading()),
        Span::styled(
            format!(
                "  {} live · {} · {} not running",
                plural(app.live_count(), "repo"),
                plural(app.items.len(), "item"),
                app.cold.len()
            ),
            muted(),
        ),
    ]);
    f.render_widget(Paragraph::new(header), chunks[0]);

    let (lines, selected_line) = body(app, now);
    // Scroll so that the selected row stays in view.
    let height = chunks[2].height as usize;
    let scroll = (selected_line + 1).saturating_sub(height);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        chunks[2],
    );

    let footer = match &app.status {
        Some(status) => Span::styled(
            status.text.clone(),
            Style::default().fg(if status.error {
                Color::Red
            } else {
                Color::Green
            }),
        ),
        None => Span::styled(FOOTER_HINT, muted()),
    };
    f.render_widget(Paragraph::new(Line::from(footer)), chunks[3]);
}

#[cfg(test)]
mod tests {
    use super::super::app::test_support::*;
    use super::super::attention::test_support::{failed, pr};
    use super::*;
    use crate::daemon::state::PrSnapshot;
    use crate::discover::test_support::row;
    use crate::fanin;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::Path;

    /// Render `app` on a fixed-size screen and return each row as text.
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app, 1_000)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn line_with<'a>(lines: &'a [String], needle: &str) -> &'a str {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?} in {lines:#?}"))
    }

    #[test]
    fn items_show_repo_worktree_reason_and_age() {
        let mut app = App::default();
        add_repo(
            &mut app,
            "a",
            Some(identity("alpha")),
            vec![waiting("alpha", "feature", 940)],
        );
        add_repo(
            &mut app,
            "b",
            Some(identity("beta")),
            vec![row("feature", Path::new("/repos/beta/feature"))],
        );
        app.apply(fanin::Event::PrStatus {
            id: "b".into(),
            prs: PrSnapshot {
                prs: [("feature".to_string(), pr("OPEN", failed(), None))].into(),
                ..PrSnapshot::default()
            },
        });

        let lines = screen(&app, 80, 10);
        assert!(lines[0].starts_with("swamp overview  2 repos live · 2 items"));
        assert_eq!(lines[2], "Needs attention");
        // The same worktree name in two repositories stays distinct.
        let first = line_with(&lines, "alpha");
        assert!(first.contains("feature") && first.contains("agent waits for you"));
        assert!(first.contains("1m"), "age of 60 seconds in {first:?}");
        assert!(
            first.starts_with('▸') || first.starts_with('>'),
            "{first:?}"
        );
        let second = line_with(&lines, "beta");
        assert!(second.contains("feature") && second.contains("checks failed"));
        assert!(!lines.iter().any(|l| l.contains("Not running")));
        assert_eq!(lines[9], FOOTER_HINT);
    }

    #[test]
    fn cold_repositories_are_in_their_own_section() {
        let mut app = App::default();
        add_repo(
            &mut app,
            "a",
            Some(identity("alpha")),
            vec![waiting("alpha", "feature", 1)],
        );
        app.set_registry(vec![entry("zulu")]);

        let lines = screen(&app, 80, 10);
        let items = lines.iter().position(|l| l == "Needs attention").unwrap();
        let cold = lines.iter().position(|l| l == "Not running").unwrap();
        assert!(items < cold);
        let zulu = lines.iter().position(|l| l.contains("zulu")).unwrap();
        assert!(zulu > cold);
        assert!(lines[zulu].contains("/repos/zulu"));
    }

    #[test]
    fn the_empty_states_say_what_is_true() {
        let app = App::default();
        let lines = screen(&app, 80, 6);
        assert_eq!(lines[2], "No swamp session runs.");

        let mut app = App::default();
        add_repo(&mut app, "a", Some(identity("alpha")), Vec::new());
        let lines = screen(&app, 80, 6);
        assert_eq!(lines[2], "Nothing needs attention.");
    }

    #[test]
    fn an_unidentified_daemon_shows_the_restart_hint() {
        let mut app = App::default();
        add_repo(
            &mut app,
            "feedfacefeedface",
            None,
            vec![waiting("x", "one", 1)],
        );
        let lines = screen(&app, 100, 10);
        let item = line_with(&lines, "feedfacefeedface");
        assert!(item.contains("one") && item.contains("(no jump)"));
        assert!(lines.iter().any(|l| l == OLD_DAEMON_HINT));
    }

    #[test]
    fn a_status_message_replaces_the_key_hint() {
        let mut app = App::default();
        app.error("launch failed: no worktrees found");
        let lines = screen(&app, 80, 6);
        assert_eq!(lines[5], "launch failed: no worktrees found");
    }

    #[test]
    fn the_selected_row_stays_in_view_on_a_short_screen() {
        let mut app = App::default();
        let rows = (0..20)
            .map(|i| waiting("alpha", &format!("wt-{i:02}"), i))
            .collect();
        add_repo(&mut app, "a", Some(identity("alpha")), rows);
        app.selected = 19;
        let lines = screen(&app, 80, 8);
        assert!(line_with(&lines, "wt-19").contains("wt-19"));
        assert!(!lines.iter().any(|l| l.contains("wt-00")));
    }
}
