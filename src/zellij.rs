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
/// When `nested` is true (we're already inside a zellij session), `ZELLIJ` is
/// stripped from the child's environment so zellij allows the nested session.
pub fn new_session_with_layout(
    layout: &Path,
    _cwd: &Path,
    session: &str,
    nested: bool,
) -> Result<()> {
    let layout = layout.to_string_lossy();
    let mut cmd = Command::new("zellij");
    cmd.args(["--new-session-with-layout", &layout, "--session", session]);
    if nested {
        cmd.env_remove("ZELLIJ");
        cmd.env_remove("ZELLIJ_SESSION_NAME");
    }
    let status = cmd
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
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_string()))
        .collect())
}

pub fn attach(session: &str, nested: bool) -> Result<()> {
    let err = exec::execvp(
        "zellij",
        &["zellij", "attach", "--force-run-commands", session],
        nested,
    );
    Err(anyhow::anyhow!("exec zellij attach failed: {:?}", err))
}

// We avoid pulling in the `exec` crate; fall back to plain spawn.
mod exec {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    pub fn execvp(cmd: &str, args: &[&str], nested: bool) -> std::io::Error {
        let mut c = Command::new(cmd);
        c.args(&args[1..]);
        if nested {
            c.env_remove("ZELLIJ");
            c.env_remove("ZELLIJ_SESSION_NAME");
        }
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
}
