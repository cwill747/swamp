use super::worktrees::git_spans;
use super::{bordered_inner, truncate};
use crate::daemon::state::{
    AgentStatus, SUBAGENT_DONE_GRACE_SECS, SubagentRow, SubagentStatus, WorktreeRow,
};
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

    // Collect rows with active or recently-idle agents, or with subagents to
    // show (a background subagent can run after its parent went idle).
    let mut entries: Vec<(usize, &WorktreeRow, Vec<&SubagentRow>)> = app
        .snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| (i, r, visible_subagents(r, now)))
        .filter(|(_, r, subs)| {
            r.agent == AgentStatus::Working
                || r.agent == AgentStatus::Waiting
                || (r.agent == AgentStatus::Idle && r.agent_ts >= recent_threshold)
                || !subs.is_empty()
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

    entries.sort_by_key(|(_, r, subs)| {
        let priority = match r.agent {
            AgentStatus::Waiting => 0,
            AgentStatus::Working => 1,
            AgentStatus::Idle if subs.iter().any(|s| s.status == SubagentStatus::Running) => 1,
            AgentStatus::Idle => 2,
        };
        (priority, r.name.clone())
    });

    let lines = layout_lines(&entries, inner.height as usize);
    let mut indices = Vec::with_capacity(lines.len());
    let rows: Vec<Row> = lines
        .iter()
        .map(|line| match *line {
            AiLine::Parent(e) => {
                let (idx, r, _) = &entries[e];
                indices.push(*idx);
                parent_row(r, app.spinner_frame, now)
            }
            AiLine::Child { entry, sub, last } => {
                let (idx, _, subs) = &entries[entry];
                indices.push(*idx);
                child_row(subs[sub], last, app.spinner_frame, now)
            }
            AiLine::More { entry, hidden } => {
                indices.push(entries[entry].0);
                Row::new(vec![
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(Span::styled(format!("└ +{hidden} more"), Theme::muted())),
                ])
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2), // icon
            Constraint::Length(8), // status label
            Constraint::Min(8),    // worktree name / subagent type
            Constraint::Min(10),   // branch
            Constraint::Min(8),    // git
            Constraint::Length(6), // elapsed
            Constraint::Fill(1),   // session name / subagent description
        ],
    )
    .block(block)
    .column_spacing(1);

    f.render_widget(table, area);

    // Map each visible AI line (parent or child) to its worktree's snapshot
    // index, so a click on a subagent acts on its parent worktree.
    drop(entries);
    app.regions.ai = Some((inner, indices));
}

/// One line of the AI pane. Indices point into the sorted entry list and its
/// visible subagents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiLine {
    Parent(usize),
    Child {
        entry: usize,
        sub: usize,
        last: bool,
    },
    More {
        entry: usize,
        hidden: usize,
    },
}

/// Subagents the pane shows for a row: all running ones, plus done ones still
/// inside the grace period. The daemon expires done subagents on a slower
/// tick, so the pane applies the grace period itself.
fn visible_subagents(r: &WorktreeRow, now: u64) -> Vec<&SubagentRow> {
    r.subagents
        .iter()
        .filter(|s| match s.status {
            SubagentStatus::Running => true,
            SubagentStatus::Done => {
                now.saturating_sub(s.finished_ts.unwrap_or(s.last_ts)) <= SUBAGENT_DONE_GRACE_SECS
            }
        })
        .collect()
}

/// Fit parent and child lines into `height`. Parent lines take priority, so
/// one busy agent cannot push other worktrees out of the pane. Child lines
/// use the space that is left, in order; a parent whose children do not all
/// fit shows a `+N more` line in place of the last ones.
fn layout_lines(
    entries: &[(usize, &WorktreeRow, Vec<&SubagentRow>)],
    height: usize,
) -> Vec<AiLine> {
    let parents = entries.len().min(height);
    let mut budget = height - parents;
    let mut lines = Vec::with_capacity(height);
    for (e, (_, _, subs)) in entries.iter().take(parents).enumerate() {
        lines.push(AiLine::Parent(e));
        let k = subs.len();
        if k == 0 || budget == 0 {
            continue;
        }
        if k <= budget {
            for i in 0..k {
                lines.push(AiLine::Child {
                    entry: e,
                    sub: i,
                    last: i + 1 == k,
                });
            }
            budget -= k;
        } else {
            let shown = budget - 1;
            for i in 0..shown {
                lines.push(AiLine::Child {
                    entry: e,
                    sub: i,
                    last: false,
                });
            }
            lines.push(AiLine::More {
                entry: e,
                hidden: k - shown,
            });
            budget = 0;
        }
    }
    lines
}

fn elapsed_since(ts: u64) -> String {
    let dur = SystemTime::now()
        .duration_since(unix_to_systemtime(ts))
        .unwrap_or(Duration::ZERO);
    format_compact_age(dur)
}

fn parent_row<'a>(r: &'a WorktreeRow, spinner_frame: usize, now: u64) -> Row<'a> {
    let is_recent = now.saturating_sub(r.agent_ts) < 300;

    // Status icon with spinner for working.
    let (icon, icon_style) = match r.agent {
        AgentStatus::Working => (
            icons::agent_icon(r.agent, spinner_frame, true),
            Style::default().fg(Theme::WORKING),
        ),
        AgentStatus::Waiting => (
            icons::agent_icon(r.agent, spinner_frame, true),
            Style::default()
                .fg(Theme::WAITING)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Idle => (
            icons::agent_icon(r.agent, spinner_frame, is_recent),
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
        elapsed_since(r.agent_ts)
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
}

fn child_row<'a>(s: &'a SubagentRow, last: bool, spinner_frame: usize, now: u64) -> Row<'a> {
    let (icon, style) = match s.status {
        SubagentStatus::Running => (
            icons::agent_icon(AgentStatus::Working, spinner_frame, true),
            Style::default().fg(Theme::WORKING),
        ),
        SubagentStatus::Done => (
            icons::agent_icon(AgentStatus::Idle, spinner_frame, true),
            Style::default().fg(Theme::IDLE_RECENT),
        ),
    };
    let glyph = if last { "└" } else { "├" };
    // Elapsed time runs from the start and stops when the subagent finishes.
    let end = match s.status {
        SubagentStatus::Running => now,
        SubagentStatus::Done => s.finished_ts.unwrap_or(s.last_ts),
    };
    let elapsed = format_compact_age(Duration::from_secs(end.saturating_sub(s.started_ts)));
    Row::new(vec![
        Cell::from(Span::styled(icon, style)),
        Cell::from(""),
        Cell::from(Line::from(vec![
            Span::styled(format!("{glyph} "), Theme::muted()),
            Span::styled(s.agent_type.as_str(), style),
        ])),
        Cell::from(""),
        Cell::from(""),
        Cell::from(Span::styled(elapsed, Theme::muted())),
        Cell::from(Span::styled(
            s.description
                .as_deref()
                .map(|d| truncate(d, 60))
                .unwrap_or_default(),
            Theme::muted(),
        )),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::resources;
    use crate::daemon::state::{PrSnapshot, Snapshot};
    use crate::tui::state::HitRegions;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn row(name: &str, agent: AgentStatus, agent_ts: u64) -> WorktreeRow {
        WorktreeRow {
            name: name.to_string(),
            path: PathBuf::from(format!("/repo/{name}")),
            branch: name.to_string(),
            upstream: None,
            upstream_gone: false,
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflict: false,
            rebase: false,
            agent,
            agent_ts,
            session_name: None,
            head_ts: 0,
            harness: None,
            is_default: false,
            removal_block: None,
            deleting: false,
            subagents: Vec::new(),
            kind: Default::default(),
            slot_state: None,
            pin: None,
        }
    }

    fn sub(
        id: &str,
        ty: &str,
        desc: Option<&str>,
        status: SubagentStatus,
        now: u64,
    ) -> SubagentRow {
        SubagentRow {
            id: id.into(),
            agent_type: ty.into(),
            description: desc.map(str::to_string),
            status,
            started_ts: now - 120,
            last_ts: now,
            finished_ts: (status == SubagentStatus::Done).then_some(now - 10),
        }
    }

    fn app(rows: Vec<WorktreeRow>) -> AppState {
        AppState {
            snapshot: Snapshot {
                rows,
                default_known: true,
            },
            selected: None,
            worktree_scroll: 0,
            spinner_frame: 0,
            repo_name: "repo".into(),
            view: crate::cli::TuiView::AiStatus,
            refreshing: false,
            pending_delete: None,
            pending_create: None,
            connected: true,
            input: None,
            status_msg: None,
            toast: None,
            resources: resources::Snapshot::default(),
            pr_snapshot: PrSnapshot::default(),
            resource_scroll: 0,
            resource_viewport_height: 0,
            current_dir: None,
            pin_cwd: false,
            tab_env: None,
            current_tab: None,
            regions: HitRegions::default(),
            last_click: None,
        }
    }

    /// Render the pane at `height` lines (borders included) and return the
    /// text of each inner line, trimmed on the right.
    fn render(app: &mut AppState, height: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(110, height)).unwrap();
        term.draw(|f| render_ai_status(f, app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        (1..height - 1)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn subagents_render_under_their_parent() {
        let now = now_unix();
        let mut parent = row("feat", AgentStatus::Working, now);
        parent.subagents = vec![
            sub(
                "a",
                "Explore",
                Some("Find hook code"),
                SubagentStatus::Running,
                now,
            ),
            sub("b", "Plan", None, SubagentStatus::Running, now),
            sub(
                "c",
                "general-purpose",
                Some("Run tests"),
                SubagentStatus::Done,
                now,
            ),
        ];
        let other = row("other", AgentStatus::Waiting, now);
        let mut app = app(vec![parent, other]);
        let lines = render(&mut app, 8);

        // Waiting sorts first; the subagents follow their own parent.
        assert!(lines[0].contains("other"), "{lines:#?}");
        assert!(lines[1].contains("feat"), "{lines:#?}");
        assert!(lines[2].contains("├ Explore") && lines[2].contains("Find hook code"));
        assert!(lines[2].contains("2m"), "elapsed since start: {lines:#?}");
        assert!(lines[3].contains("├ Plan"));
        assert!(lines[4].contains("└ general-purpose") && lines[4].contains("Run tests"));
        // The done subagent's time stops at its finish: 110s shows as 1m,
        // where a running subagent of the same age shows 2m.
        assert!(lines[4].contains("1m"), "{lines:#?}");
    }

    #[test]
    fn child_rows_map_to_parent_worktree() {
        let now = now_unix();
        let other = row("other", AgentStatus::Waiting, now);
        let mut parent = row("feat", AgentStatus::Working, now);
        parent.subagents = vec![
            sub("a", "Explore", None, SubagentStatus::Running, now),
            sub("b", "Plan", None, SubagentStatus::Running, now),
        ];
        // Snapshot order: other = 0, feat = 1. Display: other, feat, 2 children.
        let mut app = app(vec![other, parent]);
        render(&mut app, 8);
        let (_, idxs) = app.regions.ai.clone().unwrap();
        assert_eq!(idxs, vec![0, 1, 1, 1]);
    }

    #[test]
    fn idle_parent_with_running_subagent_is_shown() {
        let now = now_unix();
        // Idle well past the 5-minute recent window.
        let mut parent = row("feat", AgentStatus::Idle, now - 3600);
        parent.subagents = vec![sub(
            "a",
            "general-purpose",
            Some("Watch CI"),
            SubagentStatus::Running,
            now,
        )];
        let stale = row("stale", AgentStatus::Idle, now - 3600);
        let mut app = app(vec![parent, stale]);
        let lines = render(&mut app, 6);
        assert!(lines[0].contains("feat"), "{lines:#?}");
        assert!(lines[1].contains("└ general-purpose") && lines[1].contains("Watch CI"));
        assert!(!lines.iter().any(|l| l.contains("stale")), "{lines:#?}");
    }

    #[test]
    fn done_subagent_past_grace_is_hidden() {
        let now = now_unix();
        let mut parent = row("feat", AgentStatus::Working, now);
        let mut old = sub("a", "Plan", None, SubagentStatus::Done, now);
        old.finished_ts = Some(now - SUBAGENT_DONE_GRACE_SECS - 5);
        parent.subagents = vec![old];
        let mut app = app(vec![parent]);
        let lines = render(&mut app, 5);
        assert!(!lines.iter().any(|l| l.contains("Plan")), "{lines:#?}");
    }

    #[test]
    fn overflow_collapses_into_more_line() {
        let now = now_unix();
        let mut parent = row("feat", AgentStatus::Working, now);
        parent.subagents = (0..5)
            .map(|i| {
                sub(
                    &format!("a{i}"),
                    "Explore",
                    None,
                    SubagentStatus::Running,
                    now,
                )
            })
            .collect();
        let other = row("zeta", AgentStatus::Working, now);
        let mut app = app(vec![parent, other]);
        // 4 inner lines: 2 parents keep their lines, 2 left for children.
        let lines = render(&mut app, 6);
        assert!(lines[0].contains("feat"), "{lines:#?}");
        assert!(lines[1].contains("├ Explore"));
        assert!(lines[2].contains("└ +4 more"), "{lines:#?}");
        assert!(
            lines[3].contains("zeta"),
            "the other parent stays visible: {lines:#?}"
        );
        let (_, idxs) = app.regions.ai.clone().unwrap();
        assert_eq!(idxs, vec![0, 0, 0, 1]);
    }
}
