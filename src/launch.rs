use crate::config::{self, ConfigPaths, Harness, resolve_harness};
use crate::daemon;
use crate::daemon::socket::{ClientMsg, ServerMsg};
use crate::worktree::{
    Worktree, find_default_worktree, git_common_dir, is_bare, list_worktrees, resolve_git_dir,
};
use crate::zellij;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Returns `true` when `running` differs from `mine` (i.e. the daemon was
/// started by a different swamp build).  Simple equality for now; unit-tested
/// so future changes don't silently regress.
pub fn version_is_stale(running: &str, mine: &str) -> bool {
    running != mine
}

/// Query the running daemon for its version.  Returns `None` if the socket is
/// absent, the daemon is unreachable, or the daemon is too old to understand
/// `GetVersion`.
fn query_daemon_version(common_dir: &Path) -> Option<String> {
    let sock = daemon::socket_path(common_dir);
    if !sock.exists() {
        return None;
    }

    let handle = tokio::runtime::Handle::try_current().ok()?;
    tokio::task::block_in_place(|| {
        handle.block_on(async {
            use crate::daemon::socket::{read_server_msg, write_client_msg};
            use tokio::net::UnixStream;

            let mut stream = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                UnixStream::connect(&sock),
            )
            .await
            .ok() // Result<Result<UnixStream>, Elapsed> → Option<Result<UnixStream>>
            .and_then(|r| r.ok())?; // flatten inner Result → Option<UnixStream>

            write_client_msg(&mut stream, &ClientMsg::GetVersion)
                .await
                .ok()?;

            let resp = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                read_server_msg(&mut stream),
            )
            .await
            .ok() // Option<Result<Option<ServerMsg>>>
            .and_then(|r| r.ok()) // Option<Option<ServerMsg>>
            .and_then(|o| o)?; // Option<ServerMsg>

            match resp {
                ServerMsg::Version { version } => Some(version),
                _ => None,
            }
        })
    })
}

pub fn run(dir: Option<PathBuf>) -> Result<()> {
    let target = match dir {
        Some(p) => {
            std::fs::canonicalize(&p).with_context(|| format!("canonicalize {}", p.display()))?
        }
        None => std::env::current_dir()?,
    };
    let git_dir = resolve_git_dir(&target);
    let bare = is_bare(&git_dir);
    let worktrees = list_worktrees(&git_dir)?;
    if worktrees.is_empty() {
        anyhow::bail!("no worktrees found under {}", target.display());
    }

    let session = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "swamp".into());

    let cfg = config::ensure_configs()?;

    // When launched from inside an existing zellij session, create a *nested*
    // session rather than dumping tabs into the host session. `nested` causes
    // the spawned zellij to have ZELLIJ unset so it doesn't refuse to nest.
    let nested = zellij::in_zellij();
    spawn_new_session(&target, bare, &worktrees, &session, &cfg, nested)
}

fn spawn_new_session(
    target: &Path,
    bare: bool,
    worktrees: &[Worktree],
    session: &str,
    cfg: &ConfigPaths,
    nested: bool,
) -> Result<()> {
    // Reuse an existing session if one already matches this repo's name —
    // but first check whether the running daemon is stale.
    if let Ok(sessions) = zellij::list_sessions()
        && sessions.iter().any(|s| s == session)
    {
        let my_version = env!("CARGO_PKG_VERSION");
        let git_dir = resolve_git_dir(target);
        let common = git_common_dir(&git_dir);

        let mut do_restart = false;
        if let Ok(common) = &common {
            if let Some(running_version) = query_daemon_version(common) {
                if version_is_stale(&running_version, my_version) {
                    if std::io::stdin().is_terminal() {
                        print!(
                            "swamp: running daemon is version {} but this binary is {} — restart session? [Y/n] ",
                            running_version, my_version
                        );
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        let _ = std::io::stdin().read_line(&mut answer);
                        let answer = answer.trim().to_lowercase();
                        do_restart = answer.is_empty() || answer == "y" || answer == "yes";
                    } else {
                        eprintln!(
                            "swamp: warning: running daemon is version {} but this binary is {} (non-interactive, attaching anyway)",
                            running_version, my_version
                        );
                    }
                }
            } else {
                // No version response — treat as stale (old daemon).
                if std::io::stdin().is_terminal() {
                    print!(
                        "swamp: running daemon did not report a version (likely an older build) — restart session? [Y/n] "
                    );
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    let mut answer = String::new();
                    let _ = std::io::stdin().read_line(&mut answer);
                    let answer = answer.trim().to_lowercase();
                    do_restart = answer.is_empty() || answer == "y" || answer == "yes";
                } else {
                    eprintln!(
                        "swamp: warning: running daemon did not report a version (likely an older build), attaching anyway"
                    );
                }
            }
        }

        if do_restart {
            crate::kill::run(Some(target.to_path_buf()))?;
            // Fall through to fresh launch below.
        } else {
            return zellij::attach(session, nested);
        }
    }

    let git_dir = resolve_git_dir(target);
    let layout_path = write_multi_tab_layout(bare, worktrees, session, cfg, &git_dir)?;
    let res = zellij::new_session_with_layout(&layout_path, target, session, nested);
    let _ = std::fs::remove_file(&layout_path);
    res
}

/// Open a new zellij tab for a worktree, using a freshly generated,
/// `$SHELL`-aware layout rather than an externally-installed one.
pub fn open_worktree_tab(path: &Path, name: &str) -> Result<()> {
    let cfg = config::ensure_configs()?;
    // Resolve this worktree's harness: the repo setting, plus its persisted
    // override when the setting is `choose`.
    let override_ = git_common_dir(&resolve_git_dir(path))
        .ok()
        .map(|c| load_harness_overrides(&c))
        .and_then(|m| m.get(name).copied());
    let harness = resolve_harness(cfg.harness, override_);
    let layout = write_worktree_layout(&cfg, harness)?;
    let res = zellij::new_tab(&layout.to_string_lossy(), path, name);
    let _ = std::fs::remove_file(&layout);
    res
}

/// Close the worktree's tab and reopen it, so a harness swap takes effect live.
/// Reopening reads the freshly-persisted override, so the new tab's agent pane
/// comes up as the chosen harness.
///
/// Meant to run **detached** from the pane that triggered it (`swamp
/// relaunch-tab`): pressing `h` inside a worktree's own sidebar closes that very
/// tab, which would otherwise abort the reopen. Skipped when fewer than two tabs
/// exist — closing the only tab would end the session — so the swap then falls
/// back to applying on the next launch.
pub fn relaunch_worktree_tab(name: &str, path: &Path) -> Result<()> {
    if !zellij::in_zellij() {
        return Ok(());
    }
    let tabs = zellij::list_tab_names().unwrap_or_default();
    if !tabs.iter().any(|t| t == name) {
        // No tab to relaunch (e.g. closed); just open it fresh.
        return open_worktree_tab(path, name);
    }
    if tabs.len() < 2 {
        // Closing the sole tab would tear down the session; leave it and let the
        // persisted override apply on the next launch.
        return Ok(());
    }
    let _ = zellij::close_tab_by_name(name);
    open_worktree_tab(path, name)?;
    let _ = zellij::go_to_tab_name(name);
    Ok(())
}

fn write_multi_tab_layout(
    bare: bool,
    worktrees: &[Worktree],
    _session: &str,
    cfg: &ConfigPaths,
    git_dir: &Path,
) -> Result<PathBuf> {
    let swamp_bin = std::env::current_exe()
        .context("resolve current executable")?
        .display()
        .to_string();
    let tmp = std::env::temp_dir().join(format!("swamp-layout-{}.kdl", std::process::id()));
    let nix = nix_available();
    let mut s = String::new();
    s.push_str("layout {\n");
    s.push_str("  default_tab_template {\n");
    s.push_str("    pane size=1 borderless=true {\n");
    s.push_str("      plugin location=\"tab-bar\"\n");
    s.push_str("    }\n");
    s.push_str("    children\n");
    s.push_str("    pane size=2 borderless=true {\n");
    s.push_str("      plugin location=\"status-bar\"\n");
    s.push_str("    }\n");
    s.push_str("  }\n");

    if bare {
        s.push_str(&format!(
            "  tab name=\"dashboard\" focus=true cwd=\"{}\" {{\n",
            session_cwd(worktrees, git_dir),
        ));
        push_dashboard_panes(&mut s, cfg, &swamp_bin, nix);
        s.push_str("  }\n");
    }

    // Resume map: worktree name → recorded Claude session id. A worktree that
    // still exists and had an active session gets its Claude pane launched with
    // `claude --resume <id>` so a swamp restart picks the conversation back up
    // (#33).
    let common = git_common_dir(git_dir).ok();
    let session_ids = common.as_deref().map(load_session_ids).unwrap_or_default();
    // Per-worktree harness overrides, honored when the repo setting is `choose`.
    let harness_overrides = common
        .as_deref()
        .map(load_harness_overrides)
        .unwrap_or_default();

    for (i, wt) in worktrees.iter().enumerate() {
        let focus = if !bare && i == 0 { " focus=true" } else { "" };
        s.push_str(&format!(
            "  tab name=\"{}\"{} cwd=\"{}\" {{\n",
            wt.name(),
            focus,
            wt.path.display()
        ));
        let resume = session_ids.get(&wt.name()).map(|s| s.as_str());
        let harness = resolve_harness(cfg.harness, harness_overrides.get(&wt.name()).copied());
        push_worktree_panes(&mut s, cfg, &swamp_bin, nix, resume, harness);
        s.push_str("  }\n");
    }
    s.push_str("}\n");
    std::fs::write(&tmp, s)?;
    Ok(tmp)
}

fn session_cwd(worktrees: &[Worktree], git_dir: &Path) -> String {
    find_default_worktree(worktrees, git_dir)
        .map(|w| w.path.display().to_string())
        .unwrap_or_else(|| ".".into())
}

/// Load the worktree → Claude session id map from the persisted
/// `.swamp-status.json` in the git common dir. `swamp kill` leaves this file in
/// place, so on the next launch we can resume each worktree's session. Ids that
/// fail `is_safe_session_id` are dropped — we interpolate the id straight into a
/// shell command line, so anything outside the expected UUID charset is refused
/// rather than escaped.
fn load_session_ids(common_dir: &Path) -> std::collections::HashMap<String, String> {
    let path = common_dir.join(".swamp-status.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Default::default();
    };
    let Ok(map) =
        serde_json::from_slice::<std::collections::HashMap<String, serde_json::Value>>(&bytes)
    else {
        return Default::default();
    };
    map.into_iter()
        .filter_map(|(name, v)| {
            v.get("session_id")
                .and_then(|s| s.as_str())
                .filter(|s| is_safe_session_id(s))
                .map(|s| (name, s.to_string()))
        })
        .collect()
}

/// Load the worktree → harness override map from `.swamp-status.json`. Only
/// consulted when the repo setting is `choose`; an unrecognized value is
/// dropped so a hand-edited file can't pick a non-existent agent.
fn load_harness_overrides(common_dir: &Path) -> std::collections::HashMap<String, Harness> {
    let path = common_dir.join(".swamp-status.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Default::default();
    };
    let Ok(map) =
        serde_json::from_slice::<std::collections::HashMap<String, serde_json::Value>>(&bytes)
    else {
        return Default::default();
    };
    map.into_iter()
        .filter_map(|(name, v)| {
            let h = match v.get("harness").and_then(|s| s.as_str()) {
                Some("claude") => Harness::Claude,
                Some("codex") => Harness::Codex,
                _ => return None,
            };
            Some((name, h))
        })
        .collect()
}

/// A session id is safe to splice into a shell command only if it's a plain
/// token — Claude session ids are UUIDs, so restrict to `[A-Za-z0-9_-]`.
fn is_safe_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Generate a single-tab worktree layout for `zellij action new-tab`. Mirrors
/// the externally-installed `swamp` layout we used to depend on, but built from
/// the `$SHELL`-aware `push_worktree_panes` so it works for non-fish users.
fn write_worktree_layout(cfg: &ConfigPaths, harness: Harness) -> Result<PathBuf> {
    let swamp_bin = std::env::current_exe()
        .context("resolve current executable")?
        .display()
        .to_string();
    let tmp = std::env::temp_dir().join(format!("swamp-worktree-{}.kdl", std::process::id()));
    let mut s = String::new();
    s.push_str("layout {\n");
    // Mirror the per-tab frame from `write_multi_tab_layout`: a
    // `default_tab_template` carrying both the tab-bar (top) and status-bar
    // (bottom). A new-tab layout that omits this leaves the created tab with no
    // tab header or status bar — they only reappear when you switch to a tab
    // that *was* built with the template.
    s.push_str("  default_tab_template {\n");
    s.push_str("    pane size=1 borderless=true {\n");
    s.push_str("      plugin location=\"tab-bar\"\n");
    s.push_str("    }\n");
    s.push_str("    children\n");
    s.push_str("    pane size=2 borderless=true {\n");
    s.push_str("      plugin location=\"status-bar\"\n");
    s.push_str("    }\n");
    s.push_str("  }\n");
    s.push_str("  tab {\n");
    // A freshly-opened worktree tab has no prior session to resume.
    push_worktree_panes(&mut s, cfg, &swamp_bin, nix_available(), None, harness);
    s.push_str("  }\n");
    s.push_str("}\n");
    std::fs::write(&tmp, s)?;
    Ok(tmp)
}

/// The user's login shell, the basis for every interactive layout pane.
///
/// We launch each shell pane through `$SHELL` (falling back to bash) rather
/// than hardcoding fish, and emit the startup glue in the matching dialect.
struct Shell {
    /// Path passed as the pane's `command=`.
    path: String,
    /// The flag that runs a command string at startup: fish uses `-C` (run,
    /// then stay interactive); POSIX shells use `-c`.
    run_flag: &'static str,
    is_fish: bool,
}

fn user_shell() -> Shell {
    let path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let is_fish = Path::new(&path)
        .file_name()
        .map(|n| n == "fish")
        .unwrap_or(false);
    Shell {
        path,
        run_flag: if is_fish { "-C" } else { "-c" },
        is_fish,
    }
}

/// Whether a `nix` executable is resolvable on `$PATH`. Checked once per
/// process and cached — "decide once when the session spins up" (#34). On a
/// host without nix we skip the `nix develop` glue entirely rather than emit
/// shell that would fail with `nix: command not found` inside any repo that
/// happens to carry a `flake.nix`.
fn nix_available() -> bool {
    use std::os::unix::fs::PermissionsExt;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|d| {
                // `metadata()` follows symlinks, so a `~/.nix-profile/bin/nix`
                // link resolves; require a regular-ish file with an execute bit
                // so a stale non-executable `nix` placeholder isn't mistaken for
                // a usable install.
                std::fs::metadata(d.join("nix"))
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
        })
    })
}

/// Glue that drops into the nix dev shell when a flake/shell.nix/default.nix is
/// present and runs `in_nix` there, otherwise runs `direct` directly. Written in
/// `shell`'s dialect (fish vs POSIX). When `nix` is `false` (no nix on `$PATH`)
/// the whole detection is skipped and we just run `direct`.
///
/// `exec` controls whether the command replaces the shell (`exec …`) or runs as
/// a child so control returns afterward — the latter lets a caller chain a
/// follow-up (e.g. drop to an interactive shell once an agent exits).
fn nix_entry(shell: &Shell, nix: bool, in_nix: &str, direct: &str, exec: bool) -> String {
    let kw = if exec { "exec " } else { "" };
    if !nix {
        return format!("{kw}{direct}");
    }
    if shell.is_fish {
        format!(
            "if test -f flake.nix -o -f shell.nix -o -f default.nix; \
             if test -f .git; {kw}nix develop path:. --command {in_nix}; \
             else; {kw}nix develop --command {in_nix}; end; \
             else; {kw}{direct}; end"
        )
    } else {
        format!(
            "if [ -f flake.nix ] || [ -f shell.nix ] || [ -f default.nix ]; then \
             if [ -f .git ]; then {kw}nix develop path:. --command {in_nix}; \
             else {kw}nix develop --command {in_nix}; fi; \
             else {kw}{direct}; fi"
        )
    }
}

fn push_dashboard_panes(s: &mut String, cfg: &ConfigPaths, swamp_bin: &str, nix: bool) {
    let sh = user_shell();
    let shell_glue = nix_entry(
        &sh,
        nix,
        &format!("bash -c 'exec {}'", sh.path),
        &sh.path,
        true,
    );
    let d = &cfg.dashboard;
    s.push_str(&format!(
        r#"    pane split_direction="vertical" {{
      pane split_direction="horizontal" size="{worktrees_col}%" {{
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "worktrees"
          name "worktrees"
        }}
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "resources"
          name "resources"
        }}
      }}
      pane split_direction="horizontal" size="{ai_col}%" {{
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "ai-status"
          name "ai-status"
        }}
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "pr-status"
          name "pr-status"
        }}
      }}
      pane command="{shell_path}" size="{shell_col}%" {{
        args "{run_flag}" "{shell_glue}"
        name "shell"
      }}
    }}
"#,
        worktrees_col = d.worktrees_column,
        ai_col = d.ai_column,
        shell_col = d.shell_column,
        shell_path = sh.path,
        run_flag = sh.run_flag
    ));
}

fn push_worktree_panes(
    s: &mut String,
    cfg: &ConfigPaths,
    swamp_bin: &str,
    nix: bool,
    resume_session: Option<&str>,
    harness: Harness,
) {
    let lazygit_cfg = cfg.lazygit.display().to_string();
    let sh = user_shell();

    let lazygit_glue = if sh.is_fish {
        format!("set -gx LG_CONFIG_FILE {lazygit_cfg}; exec lazygit")
    } else {
        format!("export LG_CONFIG_FILE={lazygit_cfg}; exec lazygit")
    };

    // Resolve the agent binary on the host's PATH first, then carry that path
    // into the nix shell (whose PATH may not include it). Resume is Claude-only:
    // Codex's notify gives us no resumable id, so a Codex pane always starts
    // fresh. When a Claude session id was recorded, resume it.
    let bin = harness.bin();
    let agent_prefix = if sh.is_fish {
        format!("set -l cp (command -s {bin}); ")
    } else {
        format!("cp=$(command -v {bin}); ")
    };
    let agent_cmd = match (harness, resume_session) {
        (Harness::Claude, Some(id)) => format!("$cp --resume {id}"),
        _ => "$cp".to_string(),
    };
    let shell_glue = nix_entry(
        &sh,
        nix,
        &format!("bash -c 'exec {}'", sh.path),
        &sh.path,
        true,
    );
    // Run the agent as a child (not `exec`), then drop into an interactive nix
    // shell once it exits. So quitting the harness leaves a usable prompt in the
    // pane — you can relaunch it, or run the other harness by hand — instead of
    // a dead pane.
    let agent_glue = format!(
        "{agent_prefix}{}; {shell_glue}",
        nix_entry(&sh, nix, &agent_cmd, &agent_cmd, false)
    );

    s.push_str(&format!(
        r#"    pane split_direction="vertical" {{
      pane split_direction="horizontal" size="50%" {{
        pane command="{shell_path}" size="65%" {{
          args "{run_flag}" "{lazygit_glue}"
          name "lazygit"
        }}
        pane command="{swamp_bin}" size="35%" {{
          args "tui" "--view" "worktrees" "--pin-cwd"
          name "swamp"
        }}
      }}
      pane split_direction="horizontal" size="50%" {{
        pane command="{shell_path}" size="60%" start_suspended=true {{
          args "{run_flag}" "{agent_glue}"
          name "{bin}"
        }}
        pane command="{shell_path}" size="40%" {{
          args "{run_flag}" "{shell_glue}"
          name "shell"
        }}
      }}
    }}
"#,
        shell_path = sh.path,
        run_flag = sh.run_flag
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn version_is_stale_same_version() {
        assert!(!version_is_stale("0.1.0", "0.1.0"));
    }

    #[test]
    fn version_is_stale_different_version() {
        assert!(version_is_stale("0.1.0", "0.2.0"));
    }

    #[test]
    fn version_is_stale_empty_running() {
        // Old daemons that don't respond should be treated as stale by callers,
        // but an empty string is still different from any real version.
        assert!(version_is_stale("", "0.1.0"));
    }

    fn make_wt(path: &str, branch: &str) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            branch: branch.into(),
        }
    }

    fn dummy_git_dir() -> PathBuf {
        PathBuf::from("/nonexistent-git-dir")
    }

    #[test]
    fn session_cwd_prefers_default_branch_worktree() {
        let worktrees = vec![
            make_wt("/repo/talks/foo", "foo"),
            make_wt("/repo/talks/main", "main"),
        ];
        let cwd = session_cwd(&worktrees, &dummy_git_dir());
        assert_eq!(cwd, "/repo/talks/main");
    }

    #[test]
    fn session_cwd_falls_back_to_first() {
        let worktrees = vec![
            make_wt("/repo/talks/foo", "foo"),
            make_wt("/repo/talks/bar", "bar"),
        ];
        let cwd = session_cwd(&worktrees, &dummy_git_dir());
        assert_eq!(cwd, "/repo/talks/foo");
    }

    #[test]
    fn session_cwd_single_worktree() {
        let worktrees = vec![make_wt("/home/user/myproject", "main")];
        assert_eq!(
            session_cwd(&worktrees, &dummy_git_dir()),
            "/home/user/myproject"
        );
    }

    fn dummy_cfg() -> ConfigPaths {
        ConfigPaths {
            lazygit: PathBuf::from("/tmp/swamp/lazygit.yml"),
            dashboard: crate::config::DashboardConfig::default(),
            harness: crate::config::HarnessSetting::Claude,
        }
    }

    #[test]
    fn multi_tab_layout_dashboard_uses_default_branch_worktree() {
        let worktrees = vec![
            make_wt("/repo/talks/foo", "foo"),
            make_wt("/repo/talks/main", "main"),
        ];
        let cfg = dummy_cfg();
        let layout_path =
            write_multi_tab_layout(true, &worktrees, "talks", &cfg, &dummy_git_dir()).unwrap();
        let content = std::fs::read_to_string(&layout_path).unwrap();
        let _ = std::fs::remove_file(&layout_path);

        // The dashboard tab must have the real worktree path as its cwd.
        assert!(
            content.contains("cwd=\"/repo/talks/main\""),
            "dashboard tab should use worktree cwd; got:\n{content}"
        );
        // The bare container must NOT appear as a cwd value.
        assert!(
            !content.contains("cwd=\"/repo/talks\""),
            "dashboard tab must NOT use bare container as cwd; got:\n{content}"
        );
        // Dashboard tab should have the four swamp TUI view panes.
        for view_name in &["worktrees", "ai-status", "resources", "pr-status"] {
            assert!(
                content.contains(&format!("\"--view\" \"{}\"", view_name)),
                "dashboard tab should have --view {view_name} pane; got:\n{content}"
            );
        }
        // The layout should use the current binary, not a bare "swamp" command.
        assert!(
            !content.contains("command=\"swamp\""),
            "layout should use resolved binary path, not bare 'swamp'; got:\n{content}"
        );
        // Worktree-tab panes pin their cwd; the dashboard worktrees pane does not.
        assert!(
            content.contains("\"--view\" \"worktrees\" \"--pin-cwd\""),
            "worktree-tab pane should pass --pin-cwd; got:\n{content}"
        );
        assert_eq!(
            content.matches("--pin-cwd").count(),
            worktrees.len(),
            "exactly one --pin-cwd per worktree tab (none on dashboard); got:\n{content}"
        );
    }

    #[test]
    fn nix_entry_dialects() {
        let fish = Shell {
            path: "/usr/bin/fish".into(),
            run_flag: "-C",
            is_fish: true,
        };
        let bash = Shell {
            path: "/bin/bash".into(),
            run_flag: "-c",
            is_fish: false,
        };

        let f = nix_entry(
            &fish,
            true,
            "bash -c 'exec /usr/bin/fish'",
            "/usr/bin/fish",
            true,
        );
        assert!(
            f.contains("if test -f flake.nix") && f.trim_end().ends_with("end"),
            "fish dialect; got:\n{f}"
        );
        assert!(!f.contains("STARSHIP"), "starship glue is gone; got:\n{f}");

        let b = nix_entry(&bash, true, "bash -c 'exec /bin/bash'", "/bin/bash", true);
        assert!(
            b.contains("if [ -f flake.nix ]") && b.trim_end().ends_with("fi"),
            "posix dialect; got:\n{b}"
        );
        assert!(
            !b.contains("set -gx") && !b.contains("STARSHIP"),
            "no fish syntax / no starship; got:\n{b}"
        );

        // With nix absent from PATH (`nix=false`), no detection is emitted —
        // just a bare direct exec, in either dialect (#34).
        let nf = nix_entry(
            &fish,
            false,
            "bash -c 'exec /usr/bin/fish'",
            "/usr/bin/fish",
            true,
        );
        assert_eq!(
            nf, "exec /usr/bin/fish",
            "no-nix fish glue is a bare exec; got:\n{nf}"
        );
        let nb = nix_entry(&bash, false, "bash -c 'exec /bin/bash'", "/bin/bash", true);
        assert_eq!(
            nb, "exec /bin/bash",
            "no-nix posix glue is a bare exec; got:\n{nb}"
        );
        assert!(
            !nf.contains("nix develop") && !nb.contains("nix develop"),
            "no nix develop when nix absent"
        );

        // With `exec=false` the command runs as a child (no `exec` keyword), so a
        // caller can chain a follow-up after it returns.
        let nrun = nix_entry(&bash, false, "x", "/bin/agent", false);
        assert_eq!(nrun, "/bin/agent", "non-exec glue omits the exec keyword");
        let run = nix_entry(&bash, true, "$cp", "$cp", false);
        assert!(
            run.contains("nix develop") && !run.contains("exec "),
            "non-exec nix glue runs as a child; got:\n{run}"
        );
    }

    #[test]
    fn worktree_panes_use_env_shell_not_hardcoded_fish() {
        // Force a non-fish $SHELL and confirm the generated layout follows it.
        // SAFETY: single-threaded test; no other thread reads the environment here.
        unsafe { std::env::set_var("SHELL", "/bin/bash") };
        let mut s = String::new();
        push_worktree_panes(
            &mut s,
            &dummy_cfg(),
            "/usr/bin/swamp",
            true,
            None,
            Harness::Claude,
        );
        assert!(
            s.contains("command=\"/bin/bash\""),
            "panes should launch $SHELL; got:\n{s}"
        );
        assert!(
            !s.contains("command=\"fish\""),
            "no hardcoded fish command; got:\n{s}"
        );
        assert!(
            !s.contains("STARSHIP"),
            "starship injection is gone; got:\n{s}"
        );
        // The interactive panes (lazygit, claude, shell) all run via `-c`.
        assert!(s.contains("args \"-c\""), "bash panes use -c; got:\n{s}");
        // nix auto-entry is preserved for the shell/claude panes when nix is present.
        assert!(
            s.contains("nix develop"),
            "nix auto-entry retained; got:\n{s}"
        );

        // With nix absent (`nix=false`), no pane emits nix-entry glue (#34).
        let mut sn = String::new();
        push_worktree_panes(
            &mut sn,
            &dummy_cfg(),
            "/usr/bin/swamp",
            false,
            None,
            Harness::Claude,
        );
        assert!(
            !sn.contains("nix develop"),
            "no nix develop when nix absent; got:\n{sn}"
        );
        assert!(
            sn.contains("exec /bin/bash"),
            "shell pane execs $SHELL directly; got:\n{sn}"
        );
    }

    /// When a worktree has a recorded session id, its Claude pane resumes that
    /// session; without one it launches plain `claude` (#33).
    #[test]
    fn worktree_panes_resume_recorded_session() {
        // SAFETY: single-threaded test; no other thread reads the environment.
        unsafe { std::env::set_var("SHELL", "/bin/bash") };

        let mut with = String::new();
        push_worktree_panes(
            &mut with,
            &dummy_cfg(),
            "/usr/bin/swamp",
            false,
            Some("abc-123"),
            Harness::Claude,
        );
        assert!(
            with.contains("$cp --resume abc-123"),
            "recorded session should resume; got:\n{with}"
        );

        let mut without = String::new();
        push_worktree_panes(
            &mut without,
            &dummy_cfg(),
            "/usr/bin/swamp",
            false,
            None,
            Harness::Claude,
        );
        assert!(
            !without.contains("--resume"),
            "no session → plain claude, no --resume; got:\n{without}"
        );
    }

    /// A Codex harness resolves `codex` on PATH, names the pane `codex`, and
    /// never resumes — Codex notify gives us no resumable id.
    #[test]
    fn worktree_panes_codex_launches_codex_fresh() {
        // SAFETY: single-threaded test; no other thread reads the environment.
        unsafe { std::env::set_var("SHELL", "/bin/bash") };

        let mut s = String::new();
        push_worktree_panes(
            &mut s,
            &dummy_cfg(),
            "/usr/bin/swamp",
            false,
            Some("abc-123"),
            Harness::Codex,
        );
        assert!(
            s.contains("command -v codex"),
            "codex resolved on PATH; got:\n{s}"
        );
        assert!(s.contains("name \"codex\""), "pane named codex; got:\n{s}");
        assert!(
            !s.contains("--resume"),
            "codex never resumes even with a recorded id; got:\n{s}"
        );
        assert!(
            !s.contains("command -v claude"),
            "codex harness must not invoke claude; got:\n{s}"
        );
    }

    /// The agent runs as a child (not `exec`) and the pane drops into an
    /// interactive shell when it exits, so quitting the harness leaves a usable
    /// prompt rather than a dead pane.
    #[test]
    fn worktree_agent_pane_drops_to_shell_on_exit() {
        // SAFETY: single-threaded test; no other thread reads the environment.
        unsafe { std::env::set_var("SHELL", "/bin/bash") };

        let mut s = String::new();
        push_worktree_panes(
            &mut s,
            &dummy_cfg(),
            "/usr/bin/swamp",
            false,
            None,
            Harness::Claude,
        );
        // Agent is not exec'd, and the shell follows once it returns.
        assert!(
            s.contains("$cp; exec /bin/bash"),
            "agent runs then drops to a shell; got:\n{s}"
        );
        assert!(
            !s.contains("exec $cp"),
            "agent must not be exec'd (else the pane dies on exit); got:\n{s}"
        );
    }

    #[test]
    fn safe_session_id_accepts_uuid_rejects_shell_metachars() {
        assert!(is_safe_session_id("3f9c1e2a-7b40-4d8e-9a1f-2c3d4e5f6a7b"));
        assert!(is_safe_session_id("abc_123-DEF"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("id; rm -rf /"));
        assert!(!is_safe_session_id("$(whoami)"));
        assert!(!is_safe_session_id("a b"));
    }

    /// `load_session_ids` reads worktree → session id pairs from a persisted
    /// status file and drops entries whose id is unsafe or absent.
    #[test]
    fn load_session_ids_reads_safe_entries_only() {
        let dir = std::env::temp_dir().join(format!("swamp-sid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "feat": { "status": "idle", "ts": 1, "session_id": "good-id-1" },
            "bare": { "status": "working", "ts": 2, "session_id": "rm -rf" },
            "none": { "status": "idle", "ts": 3 }
        }"#;
        std::fs::write(dir.join(".swamp-status.json"), json).unwrap();

        let map = load_session_ids(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(map.get("feat").map(String::as_str), Some("good-id-1"));
        assert!(!map.contains_key("bare"), "unsafe id must be dropped");
        assert!(!map.contains_key("none"), "missing id must be absent");
    }

    #[test]
    fn load_session_ids_missing_file_is_empty() {
        let dir = std::env::temp_dir().join("swamp-definitely-missing-dir-xyz");
        assert!(load_session_ids(&dir).is_empty());
    }
}
