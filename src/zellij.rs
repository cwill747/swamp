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

pub fn go_to_tab_name(name: &str) -> Result<()> {
    action(&["go-to-tab-name", name])
}

pub fn close_tab_by_name(name: &str) -> Result<()> {
    go_to_tab_name(name)?;
    action(&["close-tab"])
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

/// Launch a brand-new zellij session attached to `layout`, with `cwd` and `session`.
/// Only used when *not* already inside a zellij session; the nested case switches
/// the live client over instead (see [`switch_session`]).
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

/// Switch the *calling* client to `session`. When `layout` is `Some`, the session
/// is created from that layout if it doesn't already exist; the same call then
/// moves the client into it. This is the nested-launch counterpart to `attach` /
/// `new_session_with_layout`: instead of spawning a session the host client never
/// joins, it hands the live client over to the repo session.
pub fn switch_session(session: &str, layout: Option<&Path>) -> Result<()> {
    let mut args = vec!["switch-session".to_string(), session.to_string()];
    if let Some(layout) = layout {
        args.push("--layout".to_string());
        args.push(layout.to_string_lossy().into_owned());
    }
    tracing::info!(session, layout = ?layout, "switching zellij client to session");
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    action(&refs)
}

/// Stable id of the currently active tab, parsed from `zellij action
/// current-tab-info`. The default output is `name: <n>`/`id: <n>`/`position: <n>`
/// lines; we want the `id:` value, which is the stable tab id accepted by
/// `close-tab-by-id`.
pub fn current_tab_id() -> Result<u32> {
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
    parse_tab_id(&String::from_utf8_lossy(&out.stdout))
        .context("parse tab id from current-tab-info")
}

fn parse_tab_id(stdout: &str) -> Result<u32> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("id:"))
        .map(str::trim)
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| anyhow::anyhow!("no parseable `id:` line in current-tab-info output"))
}

/// Best-effort close of tab `id` in a *named* session, targeting it explicitly so
/// it works even when the calling process's client has already switched away.
/// Failures are logged, never fatal — the originating-tab cleanup is cosmetic.
pub fn close_tab_by_id_in_session(host: &str, id: u32) {
    let id = id.to_string();
    let status = Command::new("zellij")
        .args(["--session", host, "action", "close-tab-by-id", &id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::warn!(host, id, "zellij close-tab-by-id exited {:?}", s.code()),
        Err(e) => tracing::warn!(host, id, "spawn zellij close-tab-by-id failed: {e}"),
    }
}

/// Attach to an existing session, replacing this process via `exec`. Only used
/// when *not* already inside a zellij session; the nested case switches the live
/// client over instead (see [`switch_session`]).
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
    fn parse_tab_id_reads_stable_id_line() {
        // Default `current-tab-info` output: name/id/position lines. `id:` is the
        // stable id, distinct from `position:`.
        let out = "name: nested\nid: 3\nposition: 1\n";
        assert_eq!(parse_tab_id(out).unwrap(), 3);
    }

    #[test]
    fn parse_tab_id_missing_id_is_error() {
        assert!(parse_tab_id("name: nested\nposition: 1\n").is_err());
    }
}
