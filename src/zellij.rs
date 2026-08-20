use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub fn in_zellij() -> bool {
    std::env::var("ZELLIJ").is_ok()
}

fn zellij(args: &[&str]) -> Result<()> {
    let status = Command::new("zellij")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("spawn zellij {:?}", args))?;
    if !status.success() {
        anyhow::bail!("zellij {:?} exited {:?}", args, status.code());
    }
    Ok(())
}

pub fn action(args: &[&str]) -> Result<()> {
    let mut full = vec!["action"];
    full.extend_from_slice(args);
    zellij(&full)
}

pub fn new_tab(layout: &str, cwd: &Path, name: &str) -> Result<()> {
    let cwd = cwd.to_string_lossy();
    tracing::info!(worktree = %name, layout, cwd = %cwd, "spawning zellij worktree tab");
    action(&["new-tab", "--layout", layout, "--cwd", &cwd, "--name", name])
}

fn floating_pane_args(cwd: &Path, name: &str, cmd: &[&str]) -> Vec<String> {
    let mut args = vec![
        "new-pane".to_string(),
        "--floating".to_string(),
        "--close-on-exit".to_string(),
        "--cwd".to_string(),
        cwd.to_string_lossy().into_owned(),
        "--name".to_string(),
        name.to_string(),
        "--".to_string(),
    ];
    args.extend(cmd.iter().map(|s| s.to_string()));
    args
}

/// Open a floating pane running `cmd`, closing itself when the command exits.
/// Used for the blocked-delete confirmation, which needs more room than the
/// one-line footer prompt can show — a file list and a diffstat don't fit in
/// a narrow sidebar.
///
/// Returns an error the caller can fall back from: any spawn failure (an
/// unsupported flag on an older Zellij, no Zellij at all) degrades to the
/// footer force-prompt rather than blocking the delete.
pub fn new_floating_pane(cwd: &Path, name: &str, cmd: &[&str]) -> Result<()> {
    let args = floating_pane_args(cwd, name, cmd);
    tracing::info!(name, cwd = %cwd.display(), ?cmd, "spawning zellij floating pane");
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    action(&refs)
}

pub fn go_to_tab_name(name: &str) -> Result<()> {
    action(&["go-to-tab-name", name])
}

/// Close the tab named `name`, returning focus to whatever tab was active when
/// called. No-op when no tab named `name` is open.
///
/// `close-tab` always closes the *active* tab, so the named tab has to be
/// focused first. The existence check guards the dangerous fallthrough: a
/// `go-to-tab-name` for a missing tab silently leaves focus put (it still exits
/// 0), so without the check the following `close-tab` tears down the active tab
/// instead — closing the dashboard, or the whole session when it is the last
/// tab. That fired whenever a worktree whose tab was not open got deleted.
pub fn close_tab_by_name(name: &str) -> Result<()> {
    if !list_tab_names()?.iter().any(|t| t == name) {
        return Ok(());
    }
    let origin = current_tab_name().ok().filter(|o| o != name);
    go_to_tab_name(name)?;
    action(&["close-tab"])?;
    if let Some(origin) = origin {
        let _ = go_to_tab_name(&origin);
    }
    Ok(())
}

pub fn list_tab_names() -> Result<Vec<String>> {
    let out = Command::new("zellij")
        .args(["action", "query-tab-names"])
        .output()
        .context("zellij action query-tab-names")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "zellij action query-tab-names exited {:?}: {}",
            out.status.code(),
            stderr.trim()
        );
    }
    Ok(parse_tab_names(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_tab_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Launch a brand-new Zellij session attached to `layout`, with `cwd` and `session`.
/// Zellij 0.45 treats this foreground client as a native nested session when the
/// command runs inside another Zellij session.
pub fn new_session_with_layout(layout: &Path, _cwd: &Path, session: &str) -> Result<()> {
    let layout = layout.to_string_lossy();
    tracing::info!(session, %layout, "launching zellij session from multi-tab layout");
    let status = Command::new("zellij")
        .args(["--new-session-with-layout", &layout, "--session", session])
        .status()
        .context("spawn zellij --new-session-with-layout")?;
    if !status.success() {
        anyhow::bail!("zellij session launch exited {:?}", status.code());
    }
    Ok(())
}

pub fn kill_session(name: &str) -> Result<()> {
    // kill-session terminates the session; delete-session removes the entry.
    // Both are best-effort — we warn on failure instead of bailing.
    let _ = Command::new("zellij").args(["kill-session", name]).status();
    let status = Command::new("zellij")
        .args(["delete-session", name, "--force"])
        .status()
        .context("zellij delete-session")?;
    if !status.success() {
        tracing::warn!("zellij delete-session {name:?} exited {:?}", status.code());
    }
    Ok(())
}

pub fn list_sessions() -> Result<Vec<String>> {
    let out = Command::new("zellij")
        .arg("list-sessions")
        .arg("--no-formatting")
        .output()
        .context("zellij list-sessions")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "zellij list-sessions exited {:?}: {}",
            out.status.code(),
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
        .collect())
}

fn current_tab_info() -> Result<String> {
    let out = Command::new("zellij")
        .args(["action", "current-tab-info"])
        .output()
        .context("zellij action current-tab-info")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "zellij action current-tab-info exited {:?}: {}",
            out.status.code(),
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Name of the currently active tab, parsed from the `name:` line of
/// `zellij action current-tab-info`.
pub fn current_tab_name() -> Result<String> {
    parse_tab_name(&current_tab_info()?).context("parse tab name from current-tab-info")
}

fn parse_tab_name(stdout: &str) -> Result<String> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("name:"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no parseable `name:` line in current-tab-info output"))
}

/// Attach to an existing session, replacing this process via `exec`. Zellij 0.45
/// treats the client as nested when this command runs inside another session.
pub fn attach(session: &str) -> Result<()> {
    let err = exec::execvp(
        "zellij",
        &["zellij", "attach", "--force-run-commands", session],
    );
    Err(anyhow::anyhow!("exec zellij attach failed: {:?}", err))
}

// We avoid pulling in the `exec` crate; fall back to plain spawn.
mod exec {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    pub fn execvp(cmd: &str, args: &[&str]) -> std::io::Error {
        let mut c = Command::new(cmd);
        c.args(&args[1..]);
        c.exec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tab_names_trims_blank_lines() {
        assert_eq!(
            parse_tab_names("dashboard\n main \n\nfeature\r\n"),
            vec!["dashboard", "main", "feature"]
        );
    }

    #[test]
    fn parse_tab_name_reads_name_line() {
        let out = "name: dashboard\nid: 3\nposition: 1\n";
        assert_eq!(parse_tab_name(out).unwrap(), "dashboard");
    }

    #[test]
    fn parse_tab_name_missing_or_blank_is_error() {
        assert!(parse_tab_name("id: 3\nposition: 1\n").is_err());
        assert!(parse_tab_name("name:\nid: 3\n").is_err());
    }

    #[test]
    fn floating_pane_args_wraps_command_after_separator() {
        let args = floating_pane_args(
            Path::new("/repo"),
            "delete feature",
            &[
                "swamp",
                "confirm-delete",
                "feature",
                "/repo/feature",
                "--close-tab",
            ],
        );
        assert_eq!(
            args,
            vec![
                "new-pane",
                "--floating",
                "--close-on-exit",
                "--cwd",
                "/repo",
                "--name",
                "delete feature",
                "--",
                "swamp",
                "confirm-delete",
                "feature",
                "/repo/feature",
                "--close-tab",
            ]
        );
    }
}
