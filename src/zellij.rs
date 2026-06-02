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
    action(&[
        "new-tab",
        "--layout",
        layout,
        "--cwd",
        &cwd,
        "--name",
        name,
    ])
}

pub fn go_to_tab_name(name: &str) -> Result<()> {
    action(&["go-to-tab-name", name])
}

pub fn run_floating(cmd: &str, args: &[&str], width: &str, height: &str) -> Result<()> {
    let mut full = vec![
        "action", "new-pane", "--floating", "--close-on-exit",
        "--width", width, "--height", height,
        "--", cmd,
    ];
    full.extend_from_slice(args);
    zellij(&full)
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
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect())
}

/// Launch a brand-new zellij session attached to `layout`, with `cwd` and `session`.
pub fn new_session_with_layout(layout: &Path, _cwd: &Path, session: &str) -> Result<()> {
    let layout = layout.to_string_lossy();
    let status = Command::new("zellij")
        .args([
            "--new-session-with-layout",
            &layout,
            "--session",
            session,
        ])
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
    let _ = Command::new("zellij")
        .args(["kill-session", name])
        .status();
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
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
        .collect())
}

pub fn attach(session: &str) -> Result<()> {
    let err = exec::execvp("zellij", &["zellij", "attach", "--force-run-commands", session]);
    Err(anyhow::anyhow!("exec zellij attach failed: {:?}", err))
}

// We avoid pulling in the `exec` crate; fall back to plain spawn.
mod exec {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    pub fn execvp(cmd: &str, args: &[&str]) -> std::io::Error {
        Command::new(cmd).args(&args[1..]).exec()
    }
}
