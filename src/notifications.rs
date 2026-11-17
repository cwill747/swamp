//! Best-effort OS alerts, using the same CLI tools as Herdr's system toast.

use crate::config::NotificationsConfig;
use crate::daemon::state::{AgentRecord, AgentStatus};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Waiting,
    Finished,
}

/// Derive alerts from committed parent state, not timestamps or UI refreshes.
pub fn transition(before: Option<&AgentRecord>, after: &AgentRecord) -> Option<Event> {
    if after.status == AgentStatus::Waiting
        && !before.is_some_and(|r| r.status == AgentStatus::Waiting)
    {
        Some(Event::Waiting)
    } else if after.status == AgentStatus::Idle
        && after.completion_id.is_some()
        && before.and_then(|r| r.completion_id.as_ref()) != after.completion_id.as_ref()
    {
        Some(Event::Finished)
    } else {
        None
    }
}

fn command(config: &NotificationsConfig, macos: bool, title: &str, body: &str) -> Command {
    let executable = config.command.as_deref().unwrap_or(if macos {
        "terminal-notifier"
    } else {
        "notify-send"
    });
    let mut command = Command::new(executable);
    if macos {
        command.args(["-title", "Swamp", "-subtitle", title, "-message", body]);
    } else {
        // `--` prevents branch names beginning with '-' becoming options.
        command.args(["-a", "Swamp", "-u", "normal", "--", title, body]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn run_command(mut command: Command, timeout: Duration) -> Result<()> {
    let mut child = command.spawn().context("start system notifier")?;
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            anyhow::ensure!(status?.success(), "system notifier exited unsuccessfully");
            Ok(())
        }
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!("system notifier timed out");
        }
    }
}

/// Call after persistence, outside the state lock. Failures never fail a hook.
pub async fn send(common: &Path, worktree: &str, event: Event) {
    let config = match crate::config::load_config() {
        Ok(config) => config.notifications,
        Err(error) => {
            tracing::debug!("notifications: cannot load config: {error:#}");
            return;
        }
    };
    if !config.enabled
        || (event == Event::Waiting && !config.waiting)
        || (event == Event::Finished && !config.finished)
    {
        return;
    }
    let title = format!("{} / {worktree}", crate::util::repo_name_for(common));
    let body = match event {
        Event::Waiting => "Agent needs your input",
        Event::Finished => "Agent finished its turn",
    };
    if let Err(error) = run_command(
        command(&config, cfg!(target_os = "macos"), &title, body),
        Duration::from_secs(5),
    )
    .await
    {
        // Missing tools and headless hosts are normal, so don't spam warnings.
        tracing::debug!("notifications: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::apply_hook_record;

    #[test]
    fn alerts_once_per_wait_and_completion() {
        let mut record = None;
        for (status, expected) in [
            ("idle", None),
            ("working", None),
            ("waiting", Some(Event::Waiting)),
            ("waiting", None),
            ("working", None),
            ("waiting", Some(Event::Waiting)),
            ("idle", Some(Event::Finished)),
            ("idle", None),
            ("working", None),
            ("idle", Some(Event::Finished)),
        ] {
            let (next, _) = apply_hook_record(record.as_ref(), status, None, None).unwrap();
            assert_eq!(transition(record.as_ref(), &next), expected, "{status}");
            record = Some(next);
        }
    }

    #[test]
    fn native_commands_keep_untrusted_text_as_literal_arguments() {
        let title = "-repo / 'branch'; $(touch /tmp/should-not-exist)";
        let body = "message with spaces and \"quotes\"";
        let config = NotificationsConfig::default();
        for macos in [true, false] {
            let cmd = command(&config, macos, title, body);
            let args: Vec<_> = cmd
                .as_std()
                .get_args()
                .map(|s| s.to_str().unwrap())
                .collect();
            let expected = if macos {
                vec!["-title", "Swamp", "-subtitle", title, "-message", body]
            } else {
                vec!["-a", "Swamp", "-u", "normal", "--", title, body]
            };
            assert_eq!(args, expected);
            assert_eq!(
                cmd.as_std().get_program(),
                if macos {
                    "terminal-notifier"
                } else {
                    "notify-send"
                }
            );
        }
    }

    #[tokio::test]
    async fn notifier_failure_and_timeout_are_bounded() {
        let missing = Command::new("/nonexistent/swamp-notifier");
        assert!(run_command(missing, Duration::from_secs(1)).await.is_err());
        let mut failure = Command::new("sh");
        failure.args(["-c", "exit 7"]);
        assert!(run_command(failure, Duration::from_secs(1)).await.is_err());
        let mut slow = Command::new("sleep");
        slow.arg("10").kill_on_drop(true);
        assert!(run_command(slow, Duration::from_millis(20)).await.is_err());
    }
}
