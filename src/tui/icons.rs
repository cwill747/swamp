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
            if ascii { "?" } else { "\u{f252}" }
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
