use crate::config::{ConfigPaths, Harness, resolve_harness};
use crate::worktree::{Worktree, find_default_worktree, git_common_dir};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(super) fn write_multi_tab_layout(
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
    let session_ids = common
        .as_deref()
        .map(super::load_session_ids)
        .unwrap_or_default();
    // Per-worktree harness overrides, honored when the repo setting is `choose`.
    let harness_overrides = common
        .as_deref()
        .map(super::load_harness_overrides)
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
    let tab_names: Vec<String> = worktrees.iter().map(|w| w.name()).collect();
    tracing::info!(
        bare,
        tabs = ?tab_names,
        "built multi-tab session layout"
    );
    Ok(tmp)
}

fn session_cwd(worktrees: &[Worktree], git_dir: &Path) -> String {
    find_default_worktree(worktrees, git_dir)
        .map(|w| w.path.display().to_string())
        .unwrap_or_else(|| ".".into())
}

/// Generate a single-tab worktree layout for `zellij action new-tab`. Mirrors
/// the externally-installed `swamp` layout we used to depend on, but built from
/// the `$SHELL`-aware `push_worktree_panes` so it works for non-fish users.
pub(super) fn write_worktree_layout(cfg: &ConfigPaths, harness: Harness) -> Result<PathBuf> {
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
            logging: crate::config::LoggingConfig::default(),
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
}
