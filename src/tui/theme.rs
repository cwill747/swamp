use ratatui::style::{Color, Modifier, Style};

pub struct Theme;

impl Theme {
    pub const ACCENT: Color = Color::Cyan;
    pub const WAITING: Color = Color::Yellow;
    pub const WORKING: Color = Color::Cyan;
    pub const IDLE_RECENT: Color = Color::Green;
    pub const IDLE_STALE: Color = Color::DarkGray;
    pub const DIRTY: Color = Color::Red;
    pub const MUTED: Color = Color::DarkGray;
    pub const BRANCH: Color = Color::Magenta;
    /// Accent for the default-branch (trunk) row, distinct from `BRANCH`
    /// (magenta) so the repository trunk is immediately recognizable. Gold.
    pub const DEFAULT_BRANCH: Color = Color::Indexed(178);
    pub const SELECTED_BG: Color = Color::Indexed(236);
    pub const CURRENT_BG: Color = Color::Indexed(235);

    pub fn accent_bold() -> Style {
        Style::default()
            .fg(Self::ACCENT)
            .add_modifier(Modifier::BOLD)
    }
    pub fn muted() -> Style {
        Style::default().fg(Self::MUTED)
    }
    pub fn selected() -> Style {
        Style::default()
            .bg(Self::SELECTED_BG)
            .add_modifier(Modifier::BOLD)
    }
    pub fn current() -> Style {
        Style::default().bg(Self::CURRENT_BG)
    }
}
