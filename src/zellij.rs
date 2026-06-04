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

/// Single-quote a token for safe embedding in a `bash -c` string.
fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The user's login shell, falling back to bash.
fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
}

/// Open a floating pane running `cmd args` inside `cwd`.
///
/// The command is run through the user's own login shell (`$SHELL`) so their
/// full PATH and environment is present — without it, an interactive
/// `git wt add`/`git wt remove` can fail with `git: 'wt' is not a git command`
/// the instant the pane opens. We also `cd` into `cwd` explicitly (a login
/// shell may start elsewhere) so git can find the repo, and hold the pane open
/// on a non-zero exit so the error stays visible instead of `--close-on-exit`
/// tearing the pane down immediately. The pause glue is written in the user's
/// shell dialect (fish vs POSIX).
pub fn run_floating(cmd: &str, args: &[&str], cwd: &Path, width: &str, height: &str) -> Result<()> {
    let mut cmdline = sh_quote(cmd);
    for a in args {
        cmdline.push(' ');
        cmdline.push_str(&sh_quote(a));
    }
    let shell = user_shell();
    let is_fish = Path::new(&shell)
        .file_name()
        .map(|n| n == "fish")
        .unwrap_or(false);
    let cwd_q = sh_quote(&cwd.to_string_lossy());
    let script = if is_fish {
        format!(
            "cd {cwd_q} && {cmdline}; set rc $status; \
             if test $rc -ne 0; echo; echo \"[{name} exited $rc - press enter to close]\"; read swamp_close; end",
            name = cmd,
        )
    } else {
        format!(
            "cd {cwd_q} && {cmdline}; rc=$?; \
             if [ $rc -ne 0 ]; then echo; echo \"[{name} exited $rc - press enter to close]\"; read _; fi",
            name = cmd,
        )
    };
    let full = vec![
        "action", "new-pane", "--floating", "--close-on-exit",
        "--width", width, "--height", height,
        "--", shell.as_str(), "-l", "-c", script.as_str(),
    ];
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
