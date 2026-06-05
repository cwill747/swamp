use crate::daemon::resources;
use crate::tui::AppState;
use crate::tui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

pub(super) fn render_resources(f: &mut Frame, app: &mut AppState, area: Rect) {
    let res = &app.resources;
    let title = Span::styled(
        " Resources ",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::MUTED))
        .title(title);

    app.regions.resources = Some(area);

    if res.session_pid.is_none() && res.procs.is_empty() {
        let lines = vec![Line::from(Span::styled("sampling…", Theme::muted()))];
        f.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Header row 1: session totals.
    let session_label = match res.session_pid {
        Some(pid) => format!("session pid {pid}"),
        None => "session: not found".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled("CPU ", Theme::muted()),
        Span::styled(
            format!("{:>5.1}%", res.total_cpu),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("RSS ", Theme::muted()),
        Span::styled(
            format!("{:>6}", resources::fmt_bytes(res.total_rss_bytes)),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("time ", Theme::muted()),
        Span::styled(
            resources::fmt_duration(res.total_user_time_secs),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled(
            format!("({} procs · {session_label})", res.procs.len()),
            Theme::muted(),
        ),
    ]));

    // Header row 2: system memory + load.
    let mem_pct = if res.mem_total_bytes > 0 {
        (res.mem_used_bytes as f64 / res.mem_total_bytes as f64) * 100.0
    } else {
        0.0
    };
    lines.push(Line::from(vec![
        Span::styled("mem ", Theme::muted()),
        Span::styled(
            format!(
                "{}/{} ({:.0}%)",
                resources::fmt_bytes(res.mem_used_bytes),
                resources::fmt_bytes(res.mem_total_bytes),
                mem_pct
            ),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("load ", Theme::muted()),
        Span::styled(
            format!("{:.2} {:.2} {:.2}", res.load1, res.load5, res.load15),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(""));

    // Column header.
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:>6}  {:>5}  {:>6}  {:>7}  ", "PID", "CPU%", "RSS", "TIME"),
            Theme::muted(),
        ),
        Span::styled("COMMAND", Theme::muted()),
    ]));

    for p in &res.procs {
        lines.push(Line::from(vec![
            Span::raw(format!("{:>6}  ", p.pid)),
            Span::styled(
                format!("{:>5.1}  ", p.cpu),
                if p.cpu > 10.0 {
                    Style::default()
                        .fg(Theme::WORKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::raw(format!("{:>6}  ", resources::fmt_bytes(p.rss_kb * 1024))),
            Span::raw(format!("{:>7}  ", resources::fmt_duration(p.time_secs))),
            Span::styled(p.comm.clone(), Style::default().fg(Color::White)),
        ]));
    }

    let viewport_height = area.height.saturating_sub(2);
    app.resource_viewport_height = viewport_height;
    let max = max_resource_scroll(&app.resources, viewport_height);
    app.resource_scroll = app.resource_scroll.min(max);

    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.resource_scroll, 0)),
        area,
    );
}

pub(super) fn resource_content_height(res: &resources::Snapshot) -> u16 {
    if res.session_pid.is_none() && res.procs.is_empty() {
        return 0;
    }
    (4 + res.procs.len()) as u16
}

pub fn max_resource_scroll(res: &resources::Snapshot, viewport_height: u16) -> u16 {
    let content = resource_content_height(res);
    content.saturating_sub(viewport_height)
}
