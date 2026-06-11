use crate::daemon::state::AgentStatus;
use crate::util::ascii_mode;

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPINNER_ASCII: &[&str] = &["|", "/", "-", "\\"];

pub fn agent_icon(status: AgentStatus, frame: usize, recent: bool) -> &'static str {
    let ascii = ascii_mode();
    match status {
        AgentStatus::Working => {
            if ascii {
                SPINNER_ASCII[frame % SPINNER_ASCII.len()]
            } else {
                SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
            }
        }
        AgentStatus::Waiting => {
            if ascii {
                "?"
            } else {
                "\u{f252}"
            }
        }
        AgentStatus::Idle => {
            if !recent {
                " "
            } else if ascii {
                "v"
            } else {
                "\u{f058}"
            }
        }
    }
}

pub fn dirty_marker() -> &'static str {
    if ascii_mode() { "*" } else { "\u{f03eb}" }
}

pub fn current_marker() -> &'static str {
    if ascii_mode() { ">" } else { "▸" }
}

pub fn pr_icon(state: &str, is_draft: bool) -> &'static str {
    if is_draft {
        return if ascii_mode() { "d" } else { "\u{f4dd}" };
    }
    match state {
        "OPEN" => {
            if ascii_mode() {
                "o"
            } else {
                "\u{f407}"
            }
        }
        "MERGED" => {
            if ascii_mode() {
                "m"
            } else {
                "\u{f419}"
            }
        }
        "CLOSED" => {
            if ascii_mode() {
                "x"
            } else {
                "\u{f4dc}"
            }
        }
        _ => "?",
    }
}

pub fn check_success() -> &'static str {
    if ascii_mode() { "v" } else { "\u{f058}" }
}

pub fn check_failure() -> &'static str {
    if ascii_mode() { "x" } else { "\u{f057}" }
}

pub fn review_commented() -> &'static str {
    if ascii_mode() { "c" } else { "\u{f075}" }
}

pub fn review_changes() -> &'static str {
    if ascii_mode() { "!" } else { "\u{f075}" }
}

pub fn review_approved() -> &'static str {
    if ascii_mode() { "A" } else { "\u{f00c}" }
}

/// A static, muted "PR data still loading" mark for the worktrees pane. This is
/// deliberately NOT the [`SPINNER_FRAMES`] braille spinner used for CI-pending
/// checks — a network PR fetch in flight must read differently from a build that
/// is actively running.
pub fn pr_loading() -> &'static str {
    if ascii_mode() { "..." } else { "…" }
}
