use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Child, Command};

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

/// Environment variables that tie a `zellij` command to the session that
/// contains this process. A command that targets a different session must not
/// inherit them.
const SESSION_ENV: [&str; 3] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];

/// One tab of a session, from `zellij action list-tabs --json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TabInfo {
    /// Stable ID for the `*-by-id` actions. A session resurrection assigns
    /// new IDs, so resolve the ID again before each use.
    pub tab_id: u32,
    /// Zero-based position in the tab bar.
    pub position: usize,
    pub name: String,
}

fn parse_tabs_json(stdout: &str) -> Result<Vec<TabInfo>> {
    serde_json::from_str(stdout.trim()).context("parse `zellij action list-tabs --json` output")
}

/// Find the tab of worktree `name` among `tabs`. The comparison uses the
/// canonical tab name, so a tab with an attention badge matches.
pub fn find_tab<'a>(tabs: &'a [TabInfo], name: &str) -> Option<&'a TabInfo> {
    tabs.iter()
        .find(|tab| crate::badge::canonical_tab_name(&tab.name) == name)
}

/// Run `zellij --session <session> <args>` and return its standard output.
///
/// Zellij exits with status 0 for an action against a session that does not
/// exist, so the caller must check [`session_is_live`] first.
fn in_session(session: &str, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("zellij");
    for var in SESSION_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .args(["--session", session])
        .args(args)
        .output()
        .with_context(|| format!("spawn zellij --session {session} {args:?}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "zellij --session {session} {args:?} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The tabs of `session`, which can be a session other than the current one.
pub fn list_tabs(session: &str) -> Result<Vec<TabInfo>> {
    parse_tabs_json(&in_session(session, &["action", "list-tabs", "--json"])?)
}

fn parse_client_count(stdout: &str) -> usize {
    // The first line is the column header.
    stdout
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// Number of clients that are attached to `session`.
pub fn client_count(session: &str) -> Result<usize> {
    Ok(parse_client_count(&in_session(
        session,
        &["action", "list-clients"],
    )?))
}

/// Attach to `session` in this terminal and wait until the client detaches
/// or the session ends.
pub fn attach_and_wait(session: &str) -> Result<()> {
    let mut cmd = Command::new("zellij");
    for var in SESSION_ENV {
        cmd.env_remove(var);
    }
    let status = cmd
        .args(["attach", "--force-run-commands", session])
        .status()
        .context("spawn zellij attach")?;
    if !status.success() {
        anyhow::bail!("zellij attach {session} exited {:?}", status.code());
    }
    Ok(())
}

/// Open a tab in `session`, which can be a session other than the current one.
pub fn new_tab_in(session: &str, layout: &str, cwd: &Path, name: &str) -> Result<()> {
    let cwd = cwd.to_string_lossy();
    tracing::info!(session, worktree = %name, layout, cwd = %cwd, "spawning zellij worktree tab");
    in_session(
        session,
        &[
            "action", "new-tab", "--layout", layout, "--cwd", &cwd, "--name", name,
        ],
    )
    .map(drop)
}

/// Focus the tab with ID `tab_id` in `session`.
pub fn go_to_tab_by_id(session: &str, tab_id: u32) -> Result<()> {
    in_session(session, &["action", "go-to-tab-by-id", &tab_id.to_string()]).map(drop)
}

fn switch_session_args(target: &str, tab_position: Option<usize>) -> Vec<String> {
    let mut args = vec!["switch-session".to_string(), target.to_string()];
    if let Some(position) = tab_position {
        // `--tab-position` is 1-based. `TabInfo::position` is 0-based.
        args.push("--tab-position".to_string());
        args.push((position + 1).to_string());
    }
    args
}

/// Move the Zellij client of this pane to session `target`. With
/// `tab_position` (a [`TabInfo::position`]), that tab gets the focus.
pub fn switch_session(target: &str, tab_position: Option<usize>) -> Result<()> {
    let args = switch_session_args(target, tab_position);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    action(&refs)
}

/// Start session `session` without a client. With `layout`, the session
/// starts from that layout. When an exited session with that name exists,
/// Zellij resurrects it and ignores the layout.
pub fn create_background_session(session: &str, layout: Option<&Path>, cwd: &Path) -> Result<()> {
    let mut cmd = Command::new("zellij");
    for var in SESSION_ENV {
        cmd.env_remove(var);
    }
    cmd.args([
        "attach",
        "--create-background",
        "--force-run-commands",
        session,
    ]);
    if let Some(layout) = layout {
        cmd.args(["options", "--default-layout"]).arg(layout);
    }
    tracing::info!(
        session,
        ?layout,
        "starting zellij session in the background"
    );
    let out = cmd
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .context("spawn zellij attach --create-background")?;
    if !out.status.success() {
        anyhow::bail!(
            "zellij attach --create-background {session} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The tabs of the session that contains this process.
fn list_tabs_here() -> Result<Vec<TabInfo>> {
    let out = Command::new("zellij")
        .args(["action", "list-tabs", "--json"])
        .output()
        .context("zellij action list-tabs")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "zellij action list-tabs exited {:?}: {}",
            out.status.code(),
            stderr.trim()
        );
    }
    parse_tabs_json(&String::from_utf8_lossy(&out.stdout))
}

/// Focus the tab of worktree `name` in the current session.
///
/// The tab is found by its canonical name, so a tab with an attention badge
/// matches, and the focus moves by tab ID. A tab that was opened a moment ago
/// can be absent from the first list, so the lookup repeats for a short time.
pub fn go_to_tab_name(name: &str) -> Result<()> {
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if let Some(tab) = find_tab(&list_tabs_here()?, name) {
            return action(&["go-to-tab-by-id", &tab.tab_id.to_string()]);
        }
    }
    anyhow::bail!("no open tab for {name}")
}

/// Close the tab of worktree `name` in the current session. No-op when no
/// such tab is open.
///
/// The tab is found by its canonical name and closed by its ID, so the focus
/// of the caller does not change and no other tab can close by mistake.
pub fn close_tab_by_name(name: &str) -> Result<()> {
    match find_tab(&list_tabs_here()?, name) {
        Some(tab) => action(&["close-tab-by-id", &tab.tab_id.to_string()]),
        None => Ok(()),
    }
}

/// Canonical name of each open tab in the current session, in tab order.
pub fn list_tab_names() -> Result<Vec<String>> {
    Ok(canonical_names(&list_tabs_here()?))
}

fn canonical_names(tabs: &[TabInfo]) -> Vec<String> {
    tabs.iter()
        .map(|tab| crate::badge::canonical_tab_name(&tab.name).to_string())
        .collect()
}

/// Rename the tab with ID `tab_id` in `session`.
pub fn rename_tab_by_id(session: &str, tab_id: u32, name: &str) -> Result<()> {
    in_session(
        session,
        &["action", "rename-tab-by-id", &tab_id.to_string(), name],
    )
    .map(drop)
}

/// Launch a brand-new Zellij session attached to `layout`, with `cwd` and `session`.
/// Zellij 0.45 treats this foreground client as a native nested session when the
/// command runs inside another Zellij session.
pub fn spawn_new_session_with_layout(layout: &Path, _cwd: &Path, session: &str) -> Result<Child> {
    let layout = layout.to_string_lossy();
    tracing::info!(session, %layout, "launching zellij session from multi-tab layout");
    Command::new("zellij")
        .args(["--new-session-with-layout", &layout, "--session", session])
        .spawn()
        .context("spawn zellij --new-session-with-layout")
}

pub fn wait_for_session_exit(mut child: Child) -> Result<()> {
    let status = child.wait().context("wait for zellij session")?;
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

/// State of a session that `zellij list-sessions` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Live,
    /// The session ended and Zellij can resurrect it.
    Exited,
}

fn parse_sessions(stdout: &str) -> Vec<(String, SessionState)> {
    stdout
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            let state = if line.contains("(EXITED") {
                SessionState::Exited
            } else {
                SessionState::Live
            };
            Some((name.to_string(), state))
        })
        .collect()
}

fn list_session_states() -> Result<Vec<(String, SessionState)>> {
    let out = Command::new("zellij")
        .arg("list-sessions")
        .arg("--no-formatting")
        .output()
        .context("zellij list-sessions")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // With no session at all, Zellij reports that on stderr and fails.
        if stderr.contains("No active zellij sessions") {
            return Ok(Vec::new());
        }
        anyhow::bail!(
            "zellij list-sessions exited {:?}: {}",
            out.status.code(),
            stderr.trim()
        );
    }
    Ok(parse_sessions(&String::from_utf8_lossy(&out.stdout)))
}

/// Each session name, including exited sessions that Zellij can resurrect.
pub fn list_sessions() -> Result<Vec<String>> {
    Ok(list_session_states()?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// The state of session `name`, or `None` when Zellij does not list it.
pub fn session_state(name: &str) -> Result<Option<SessionState>> {
    Ok(list_session_states()?
        .into_iter()
        .find(|(session, _)| session == name)
        .map(|(_, state)| state))
}

/// Remove the record of an exited session, so that the name is free.
pub fn delete_session(name: &str) -> Result<()> {
    zellij(&["delete-session", name, "--force"])
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
    fn parse_tabs_json_reads_id_position_and_name() {
        // Captured from `zellij action list-tabs --json` (Zellij 0.45.0).
        let out = r#"[
  {
    "position": 0,
    "name": "dashboard",
    "active": false,
    "panes_to_hide": 0,
    "is_fullscreen_active": false,
    "is_sync_panes_active": false,
    "are_floating_panes_visible": false,
    "other_focused_clients": [],
    "active_swap_layout_name": null,
    "is_swap_layout_dirty": false,
    "viewport_rows": 49,
    "viewport_columns": 50,
    "display_area_rows": 50,
    "display_area_columns": 50,
    "selectable_tiled_panes_count": 1,
    "selectable_floating_panes_count": 0,
    "tab_id": 0,
    "has_bell_notification": false,
    "is_flashing_bell": false
  },
  {
    "position": 1,
    "name": "my feature",
    "active": true,
    "tab_id": 7
  }
]
"#;
        let tabs = parse_tabs_json(out).unwrap();
        assert_eq!(
            tabs,
            [
                TabInfo {
                    tab_id: 0,
                    position: 0,
                    name: "dashboard".into()
                },
                TabInfo {
                    tab_id: 7,
                    position: 1,
                    name: "my feature".into()
                },
            ]
        );
        assert_eq!(find_tab(&tabs, "my feature").map(|t| t.tab_id), Some(7));
        assert_eq!(find_tab(&tabs, "feature"), None);
    }

    #[test]
    fn parse_tabs_json_rejects_a_session_not_found_message() {
        // Zellij prints this, with exit status 0, for a missing session.
        assert!(
            parse_tabs_json("Session 'gone' not found. The following sessions are active:")
                .is_err()
        );
        assert!(parse_tabs_json("").is_err());
    }

    #[test]
    fn parse_client_count_skips_the_header() {
        let out = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                   1         terminal_0     N/A            \n\
                   2         plugin_3       zellij:about   \n";
        assert_eq!(parse_client_count(out), 2);
        assert_eq!(
            parse_client_count("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n"),
            0
        );
        assert_eq!(parse_client_count(""), 0);
    }

    #[test]
    fn switch_session_position_is_one_based() {
        assert_eq!(
            switch_session_args("repo-1a2b", None),
            ["switch-session", "repo-1a2b"]
        );
        assert_eq!(
            switch_session_args("repo-1a2b", Some(0)),
            ["switch-session", "repo-1a2b", "--tab-position", "1"]
        );
        assert_eq!(
            switch_session_args("repo-1a2b", Some(2)),
            ["switch-session", "repo-1a2b", "--tab-position", "3"]
        );
    }

    #[test]
    fn parse_sessions_separates_live_from_exited() {
        let out = "GHOSTS-4e6b [Created 4h 46m 44s ago] \n\
                   backstage-d365 [Created 26days ago] (EXITED - attach to resurrect)\n\n";
        assert_eq!(
            parse_sessions(out),
            [
                ("GHOSTS-4e6b".to_string(), SessionState::Live),
                ("backstage-d365".to_string(), SessionState::Exited),
            ]
        );
    }

    #[test]
    fn a_badged_tab_is_found_by_its_canonical_name() {
        let tabs = vec![
            TabInfo {
                tab_id: 0,
                position: 0,
                name: "dashboard ●2".into(),
            },
            TabInfo {
                tab_id: 4,
                position: 1,
                name: "● feature".into(),
            },
            TabInfo {
                tab_id: 5,
                position: 2,
                name: "main".into(),
            },
        ];
        assert_eq!(find_tab(&tabs, "feature").map(|t| t.tab_id), Some(4));
        assert_eq!(find_tab(&tabs, "dashboard").map(|t| t.tab_id), Some(0));
        assert_eq!(find_tab(&tabs, "main").map(|t| t.tab_id), Some(5));
        assert_eq!(find_tab(&tabs, "● feature"), None);
        assert_eq!(canonical_names(&tabs), ["dashboard", "feature", "main"]);
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
