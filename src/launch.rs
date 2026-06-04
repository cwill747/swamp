use crate::config::{self, ConfigPaths};
use crate::daemon;
use crate::daemon::socket::{ClientMsg, ServerMsg};
use crate::worktree::{find_default_worktree, git_common_dir, is_bare, list_worktrees, resolve_git_dir, Worktree};
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
    tokio::task::block_in_place(|| handle.block_on(async {
        use crate::daemon::socket::{read_server_msg, write_client_msg};
        use tokio::net::UnixStream;

        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            UnixStream::connect(&sock),
        )
        .await
        .ok()  // Result<Result<UnixStream>, Elapsed> → Option<Result<UnixStream>>
        .and_then(|r| r.ok())?; // flatten inner Result → Option<UnixStream>

        write_client_msg(&mut stream, &ClientMsg::GetVersion)
            .await
            .ok()?;

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_server_msg(&mut stream),
        )
        .await
        .ok()                         // Option<Result<Option<ServerMsg>>>
        .and_then(|r| r.ok())         // Option<Option<ServerMsg>>
        .and_then(|o| o)?;            // Option<ServerMsg>

        match resp {
            ServerMsg::Version { version } => Some(version),
            _ => None,
        }
    }))
}

pub fn run(dir: Option<PathBuf>) -> Result<()> {
    let target = match dir {
        Some(p) => std::fs::canonicalize(&p)
            .with_context(|| format!("canonicalize {}", p.display()))?,
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
    if let Ok(sessions) = zellij::list_sessions() {
        if sessions.iter().any(|s| s == session) {
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
    let layout = write_worktree_layout(&cfg)?;
    let res = zellij::new_tab(&layout.to_string_lossy(), path, name);
    let _ = std::fs::remove_file(&layout);
    res
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

    for (i, wt) in worktrees.iter().enumerate() {
        let focus = if !bare && i == 0 { " focus=true" } else { "" };
        s.push_str(&format!(
            "  tab name=\"{}\"{} cwd=\"{}\" {{\n",
            wt.name(),
            focus,
            wt.path.display()
        ));
        push_worktree_panes(&mut s, cfg, &swamp_bin, nix);
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
            head: "abc1234".into(),
            bare: false,
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
        assert_eq!(session_cwd(&worktrees, &dummy_git_dir()), "/home/user/myproject");
    }

    fn dummy_cfg() -> ConfigPaths {
        ConfigPaths {
            lazygit: PathBuf::from("/tmp/swamp/lazygit.yml"),
        }
    }

    #[test]
    fn multi_tab_layout_dashboard_uses_default_branch_worktree() {
        let worktrees = vec![
            make_wt("/repo/talks/foo", "foo"),
            make_wt("/repo/talks/main", "main"),
        ];
        let cfg = dummy_cfg();
        let layout_path = write_multi_tab_layout(true, &worktrees, "talks", &cfg, &dummy_git_dir()).unwrap();
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
        let fish = Shell { path: "/usr/bin/fish".into(), run_flag: "-C", is_fish: true };
        let bash = Shell { path: "/bin/bash".into(), run_flag: "-c", is_fish: false };

        let f = nix_entry(&fish, true, "bash -c 'exec /usr/bin/fish'", "/usr/bin/fish");
        assert!(f.contains("if test -f flake.nix") && f.trim_end().ends_with("end"), "fish dialect; got:\n{f}");
        assert!(!f.contains("STARSHIP"), "starship glue is gone; got:\n{f}");

        let b = nix_entry(&bash, true, "bash -c 'exec /bin/bash'", "/bin/bash");
        assert!(b.contains("if [ -f flake.nix ]") && b.trim_end().ends_with("fi"), "posix dialect; got:\n{b}");
        assert!(!b.contains("set -gx") && !b.contains("STARSHIP"), "no fish syntax / no starship; got:\n{b}");

        // With nix absent from PATH (`nix=false`), no detection is emitted —
        // just a bare direct exec, in either dialect (#34).
        let nf = nix_entry(&fish, false, "bash -c 'exec /usr/bin/fish'", "/usr/bin/fish");
        assert_eq!(nf, "exec /usr/bin/fish", "no-nix fish glue is a bare exec; got:\n{nf}");
        let nb = nix_entry(&bash, false, "bash -c 'exec /bin/bash'", "/bin/bash");
        assert_eq!(nb, "exec /bin/bash", "no-nix posix glue is a bare exec; got:\n{nb}");
        assert!(!nf.contains("nix develop") && !nb.contains("nix develop"), "no nix develop when nix absent");
    }

    #[test]
    fn worktree_panes_use_env_shell_not_hardcoded_fish() {
        // Force a non-fish $SHELL and confirm the generated layout follows it.
        // SAFETY: single-threaded test; no other thread reads the environment here.
        unsafe { std::env::set_var("SHELL", "/bin/bash") };
        let mut s = String::new();
        push_worktree_panes(&mut s, &dummy_cfg(), "/usr/bin/swamp", true);
        assert!(s.contains("command=\"/bin/bash\""), "panes should launch $SHELL; got:\n{s}");
        assert!(!s.contains("command=\"fish\""), "no hardcoded fish command; got:\n{s}");
        assert!(!s.contains("STARSHIP"), "starship injection is gone; got:\n{s}");
        // The interactive panes (lazygit, claude, shell) all run via `-c`.
        assert!(s.contains("args \"-c\""), "bash panes use -c; got:\n{s}");
        // nix auto-entry is preserved for the shell/claude panes when nix is present.
        assert!(s.contains("nix develop"), "nix auto-entry retained; got:\n{s}");

        // With nix absent (`nix=false`), no pane emits nix-entry glue (#34).
        let mut sn = String::new();
        push_worktree_panes(&mut sn, &dummy_cfg(), "/usr/bin/swamp", false);
        assert!(!sn.contains("nix develop"), "no nix develop when nix absent; got:\n{sn}");
        assert!(sn.contains("exec /bin/bash"), "shell pane execs $SHELL directly; got:\n{sn}");
    }
}

/// Generate a single-tab worktree layout for `zellij action new-tab`. Mirrors
/// the externally-installed `swamp` layout we used to depend on, but built from
/// the `$SHELL`-aware `push_worktree_panes` so it works for non-fish users.
fn write_worktree_layout(cfg: &ConfigPaths) -> Result<PathBuf> {
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
    push_worktree_panes(&mut s, cfg, &swamp_bin, nix_available());
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
/// present and `exec`s `in_nix` there, otherwise `exec`s `direct` directly.
/// Written in `shell`'s dialect (fish vs POSIX). When `nix` is `false` (no nix
/// on `$PATH`) the whole detection is skipped and we just `exec` `direct`.
fn nix_entry(shell: &Shell, nix: bool, in_nix: &str, direct: &str) -> String {
    if !nix {
        return format!("exec {direct}");
    }
    if shell.is_fish {
        format!(
            "if test -f flake.nix -o -f shell.nix -o -f default.nix; \
             if test -f .git; exec nix develop path:. --command {in_nix}; \
             else; exec nix develop --command {in_nix}; end; \
             else; exec {direct}; end"
        )
    } else {
        format!(
            "if [ -f flake.nix ] || [ -f shell.nix ] || [ -f default.nix ]; then \
             if [ -f .git ]; then exec nix develop path:. --command {in_nix}; \
             else exec nix develop --command {in_nix}; fi; \
             else exec {direct}; fi"
        )
    }
}

fn push_dashboard_panes(s: &mut String, _cfg: &ConfigPaths, swamp_bin: &str, nix: bool) {
    let sh = user_shell();
    let shell_glue = nix_entry(&sh, nix, &format!("bash -c 'exec {}'", sh.path), &sh.path);
    s.push_str(&format!(r#"    pane split_direction="vertical" {{
      pane split_direction="horizontal" size="33%" {{
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "worktrees"
          name "worktrees"
        }}
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "resources"
          name "resources"
        }}
      }}
      pane split_direction="horizontal" size="34%" {{
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "ai-status"
          name "ai-status"
        }}
        pane command="{swamp_bin}" size="50%" {{
          args "tui" "--view" "pr-status"
          name "pr-status"
        }}
      }}
      pane command="{shell_path}" size="33%" {{
        args "{run_flag}" "{shell_glue}"
        name "shell"
      }}
    }}
"#, shell_path = sh.path, run_flag = sh.run_flag));
}

fn push_worktree_panes(s: &mut String, cfg: &ConfigPaths, swamp_bin: &str, nix: bool) {
    let lazygit_cfg = cfg.lazygit.display().to_string();
    let sh = user_shell();

    let lazygit_glue = if sh.is_fish {
        format!("set -gx LG_CONFIG_FILE {lazygit_cfg}; exec lazygit")
    } else {
        format!("export LG_CONFIG_FILE={lazygit_cfg}; exec lazygit")
    };

    // Resolve claude on the host's PATH first, then carry that path into the
    // nix shell (whose PATH may not include it).
    let claude_prefix = if sh.is_fish {
        "set -l cp (command -s claude); "
    } else {
        "cp=$(command -v claude); "
    };
    let claude_glue = format!("{claude_prefix}{}", nix_entry(&sh, nix, "$cp", "$cp"));

    let shell_glue = nix_entry(&sh, nix, &format!("bash -c 'exec {}'", sh.path), &sh.path);

    s.push_str(&format!(r#"    pane split_direction="vertical" {{
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
          args "{run_flag}" "{claude_glue}"
          name "claude"
        }}
        pane command="{shell_path}" size="40%" {{
          args "{run_flag}" "{shell_glue}"
          name "shell"
        }}
      }}
    }}
"#, shell_path = sh.path, run_flag = sh.run_flag));
}
