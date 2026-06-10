use crate::config::{ConfigPaths, Harness};
use crate::worktree::{Worktree, find_default_worktree};
use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Escaping helpers
// ---------------------------------------------------------------------------

/// Escape a string for use inside a KDL double-quoted string.
///
/// KDL string escapes: `\` → `\\`, `"` → `\"`, newline → `\n`,
/// carriage return → `\r`, tab → `\t`. Other ASCII control characters
/// are escaped as `\u{XX}`.
fn kdl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => {
                // Remaining ASCII control chars (0x00–0x1F, 0x7F except \n\r\t).
                out.push_str(&format!("\\u{{{:02X}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// POSIX single-quote a string for use inside sh/bash command strings.
///
/// Wraps the value in `'...'`. Any embedded `'` is handled by ending
/// the single-quoted span, emitting an escaped `\'`, then restarting
/// the span: `'` → `'\''`.
fn sh_quote(s: &str) -> String {
    let mut out = String::new();
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Fish single-quote a string for use inside fish command strings.
///
/// In fish single-quoted strings, only `\` and `'` are special.
/// `\` → `\\`, `'` → `\'`.
fn fish_quote(s: &str) -> String {
    let mut out = String::new();
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------

pub(super) fn write_multi_tab_layout(
    worktrees: &[Worktree],
    _session: &str,
    cfg: &ConfigPaths,
    git_dir: &Path,
) -> Result<TempLayout> {
    let swamp_bin = std::env::current_exe()
        .context("resolve current executable")?
        .display()
        .to_string();
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

    // A new session opens to a single focused dashboard tab — for bare and
    // normal repos alike. Worktree tabs are no longer pre-created per worktree;
    // the user opens them on demand from the dashboard (see
    // `crate::launch::open_worktree_tab`), so the tab count is independent of
    // the worktree count.
    s.push_str(&format!(
        "  tab name=\"dashboard\" focus=true cwd=\"{}\" {{\n",
        kdl_escape(&session_cwd(worktrees, git_dir)),
    ));
    push_dashboard_panes(&mut s, cfg, &swamp_bin, nix);
    s.push_str("  }\n");
    s.push_str("}\n");
    let tmp = TempLayout::create("layout", &s)?;
    tracing::info!("built single dashboard-tab session layout");
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
pub(super) fn write_worktree_layout(
    cfg: &ConfigPaths,
    harness: Harness,
    resume_session: Option<&str>,
) -> Result<TempLayout> {
    let swamp_bin = std::env::current_exe()
        .context("resolve current executable")?
        .display()
        .to_string();
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
    // An on-demand worktree tab resumes its recorded Claude session when one
    // exists (`claude --resume <id>`), else starts the agent fresh.
    push_worktree_panes(
        &mut s,
        cfg,
        &swamp_bin,
        nix_available(),
        resume_session,
        harness,
    );
    s.push_str("  }\n");
    s.push_str("}\n");
    TempLayout::create("worktree", &s)
}

pub(super) struct TempLayout {
    path: PathBuf,
}

impl TempLayout {
    fn create(kind: &str, content: &str) -> Result<Self> {
        let base = layout_base_dir()?;
        for n in 0..1000 {
            let path = base.join(format!("swamp-{kind}-{}-{n}.kdl", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(content.as_bytes())
                        .with_context(|| format!("write {}", path.display()))?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
            }
        }
        anyhow::bail!("could not allocate unique swamp {kind} layout file")
    }
}

#[cfg(not(test))]
fn layout_base_dir() -> Result<PathBuf> {
    crate::util::runtime_base_dir()
}

#[cfg(test)]
fn layout_base_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("swamp-layout-test-{}", std::process::id()));
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&base)
        .with_context(|| format!("create test layout dir {}", base.display()))?;
    Ok(base)
}

impl Deref for TempLayout {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl AsRef<Path> for TempLayout {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempLayout {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
        shell_path = kdl_escape(&sh.path),
        run_flag = sh.run_flag,
        swamp_bin = kdl_escape(swamp_bin),
        shell_glue = kdl_escape(&shell_glue),
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

    // shell-quote lazygit_cfg for use inside a shell assignment, then
    // KDL-escape the whole glue string for embedding in a KDL args "…" token.
    let lazygit_cfg_shell_quoted = if sh.is_fish {
        fish_quote(&lazygit_cfg)
    } else {
        sh_quote(&lazygit_cfg)
    };
    let lazygit_glue = if sh.is_fish {
        format!(
            "set -gx LG_CONFIG_FILE {}; exec lazygit",
            lazygit_cfg_shell_quoted
        )
    } else {
        format!(
            "export LG_CONFIG_FILE={}; exec lazygit",
            lazygit_cfg_shell_quoted
        )
    };

    // Resolve the agent binary on the host's PATH first, then carry that path
    // into the nix shell (whose PATH may not include it). Resume is Claude-only:
    // Codex's notify gives us no resumable id, so a Codex pane always starts
    // fresh. When a Claude session id was recorded, resume it.
    let bin = harness.bin();
    // bin is a fixed identifier from our own Harness enum; no quoting needed for
    // `command -s`/`command -v`.  Session ids come from our own recorded values
    // (UUIDs); still quote them for defence.
    let agent_prefix = if sh.is_fish {
        format!("set -l cp (command -s {bin}); ")
    } else {
        format!("cp=$(command -v {bin}); ")
    };
    let agent_cmd = match (harness, resume_session) {
        (Harness::Claude, Some(id)) => format!("$cp --resume {}", sh_quote(id)),
        _ => "$cp".to_string(),
    };
    // shell_glue: sh.path is used as the direct-exec target and must be
    // shell-quoted when embedded in the `bash -c '...'` wrapper and as the
    // fallback `direct` path.  Here it is passed as the literal command to
    // exec, so no extra quoting layer is needed beyond what nix_entry produces —
    // but the entire resulting string is then KDL-escaped before insertion.
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
        shell_path = kdl_escape(&sh.path),
        run_flag = sh.run_flag,
        lazygit_glue = kdl_escape(&lazygit_glue),
        swamp_bin = kdl_escape(swamp_bin),
        agent_glue = kdl_escape(&agent_glue),
        bin = kdl_escape(bin),
        shell_glue = kdl_escape(&shell_glue),
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

    // Serializes tests that mutate process-global state: the $SHELL variable
    // and the shared pid-keyed layout file in the temp dir.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn multi_tab_layout_dashboard_uses_default_branch_worktree() {
        let _guard = env_guard();
        let worktrees = vec![
            make_wt("/repo/talks/foo", "foo"),
            make_wt("/repo/talks/main", "main"),
        ];
        let cfg = dummy_cfg();
        let layout_path =
            write_multi_tab_layout(&worktrees, "talks", &cfg, &dummy_git_dir()).unwrap();
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
        // No worktree tabs are pre-created at launch, so nothing passes
        // --pin-cwd (only worktree-tab panes ever do).
        assert!(
            !content.contains("--pin-cwd"),
            "launch layout must not pre-create pinned worktree tabs; got:\n{content}"
        );
        // Exactly one tab — the dashboard — regardless of worktree count.
        assert_eq!(
            content.matches("  tab name=").count(),
            1,
            "launch layout should contain only the dashboard tab; got:\n{content}"
        );
        assert!(
            content.contains("tab name=\"dashboard\""),
            "the sole tab should be the dashboard; got:\n{content}"
        );
    }

    /// The launch layout is dashboard-only: the tab count does not track the
    /// worktree count, and worktree names never appear as tabs.
    #[test]
    fn multi_tab_layout_is_single_dashboard_tab_for_many_worktrees() {
        let _guard = env_guard();
        let worktrees = vec![
            make_wt("/repo/talks/main", "main"),
            make_wt("/repo/talks/foo", "foo"),
            make_wt("/repo/talks/bar", "bar"),
            make_wt("/repo/talks/baz", "baz"),
        ];
        let cfg = dummy_cfg();
        let layout_path =
            write_multi_tab_layout(&worktrees, "talks", &cfg, &dummy_git_dir()).unwrap();
        let content = std::fs::read_to_string(&layout_path).unwrap();
        let _ = std::fs::remove_file(&layout_path);

        assert_eq!(
            content.matches("  tab name=").count(),
            1,
            "only the dashboard tab regardless of worktree count; got:\n{content}"
        );
        for wt in &["foo", "bar", "baz"] {
            assert!(
                !content.contains(&format!("tab name=\"{wt}\"")),
                "worktree {wt} must not get its own tab at launch; got:\n{content}"
            );
        }
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
        let _guard = env_guard();
        // SAFETY: ENV_LOCK serializes the tests that touch $SHELL.
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
        let _guard = env_guard();
        // SAFETY: ENV_LOCK serializes the tests that touch $SHELL.
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
            with.contains("$cp --resume 'abc-123'"),
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
        let _guard = env_guard();
        // SAFETY: ENV_LOCK serializes the tests that touch $SHELL.
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
        let _guard = env_guard();
        // SAFETY: ENV_LOCK serializes the tests that touch $SHELL.
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

    // -----------------------------------------------------------------------
    // Escaping helper unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn kdl_escape_basic() {
        assert_eq!(kdl_escape("hello"), "hello");
        assert_eq!(kdl_escape(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(kdl_escape("back\\slash"), "back\\\\slash");
        assert_eq!(kdl_escape("new\nline"), "new\\nline");
        assert_eq!(kdl_escape("tab\there"), "tab\\there");
        assert_eq!(kdl_escape("cr\rhere"), "cr\\rhere");
    }

    #[test]
    fn kdl_escape_control_chars() {
        // NULL byte → \u{00}
        assert_eq!(kdl_escape("\x00"), "\\u{00}");
        // BEL → \u{07}
        assert_eq!(kdl_escape("\x07"), "\\u{07}");
    }

    #[test]
    fn sh_quote_basic() {
        assert_eq!(sh_quote("/home/user/file"), "'/home/user/file'");
        assert_eq!(sh_quote("/path with spaces/x"), "'/path with spaces/x'");
        assert_eq!(sh_quote("it's here"), "'it'\\''s here'");
        assert_eq!(sh_quote(""), "''");
    }

    #[test]
    fn fish_quote_basic() {
        assert_eq!(fish_quote("/home/user/file"), "'/home/user/file'");
        assert_eq!(fish_quote("/path with spaces/x"), "'/path with spaces/x'");
        assert_eq!(fish_quote("it's here"), r"'it\'s here'");
        assert_eq!(fish_quote("back\\slash"), "'back\\\\slash'");
        assert_eq!(fish_quote(""), "''");
    }

    /// A worktree whose path basename contains `"` must not produce a raw `"`
    /// inside a KDL quoted string attribute — that would terminate the string
    /// early and allow KDL node injection.  `wt.name()` returns `path.file_name()`,
    /// so the hostile content lives in the last path component.
    ///
    /// Note: the hostile string must not contain `/` because `Path::file_name()`
    /// splits on path separators; we use `"` and `\` without `/`.
    #[test]
    fn hostile_branch_name_does_not_break_kdl() {
        // Path basename that tries to escape the KDL string (no `/` so
        // Path::file_name() returns the whole thing).
        let hostile_name = "feat\" focus=true cwd=\"injected";
        // Build a path whose file_name() is the hostile string.
        let hostile_path = format!("/repo/work/{hostile_name}");
        let wt = make_wt(&hostile_path, "some-branch");
        // name() returns path.file_name() — i.e., the hostile_name.
        let name = wt.name();
        assert_eq!(
            name, hostile_name,
            "sanity: name() should return the hostile basename"
        );
        let escaped = kdl_escape(&name);
        // The escaped form should contain the backslash-escaped quote sequence.
        assert!(
            escaped.contains("\\\""),
            "escaped form should contain \\\"; got: {escaped}"
        );
        // The escaped output must not contain the raw injection sequence.
        // Specifically: every `"` in the input should become `\"` in the output.
        // Verify by checking the raw input's `"` characters no longer appear
        // in an unescaped form — i.e., the input `"` count equals the `\"`
        // (backslash-double-quote) count in the output.
        let raw_quotes = name.chars().filter(|&c| c == '"').count();
        let escaped_quotes = escaped.matches("\\\"").count();
        assert_eq!(
            raw_quotes, escaped_quotes,
            "each raw '\"' should be escaped to '\\\"'; raw={raw_quotes} escaped={escaped_quotes}; \
             input={name:?} output={escaped:?}"
        );
    }

    /// A path containing spaces and a single-quote must produce shell glue that
    /// keeps the entire value inside one quoted token (no word-splitting).
    #[test]
    fn hostile_path_shell_glue_stays_quoted() {
        // Path with a space and an embedded single-quote (e.g., someone's home dir).
        let hostile_path = "/home/o'brien/my projects/lazygit.yml";

        // POSIX sh quoting
        let sh_quoted = sh_quote(hostile_path);
        // Must start and end with ' and contain no unescaped spaces outside quotes.
        assert!(
            sh_quoted.starts_with('\''),
            "sh_quote must start with '; got: {sh_quoted}"
        );
        // The value must not be split: verify by checking the whole glue string
        // has the path as one token (no whitespace outside quotes in the value).
        let sh_glue = format!("export LG_CONFIG_FILE={sh_quoted}; exec lazygit");
        // After `export LG_CONFIG_FILE=` the next token must end at `;`.
        let after_eq = sh_glue
            .strip_prefix("export LG_CONFIG_FILE=")
            .expect("prefix");
        let token_end = after_eq.find("; exec").expect("semicolon");
        let token = &after_eq[..token_end];
        assert!(
            !token.contains(' ') || token.starts_with('\''),
            "path token is properly quoted (no bare spaces); got token: {token}"
        );

        // Fish quoting
        let fish_quoted = fish_quote(hostile_path);
        assert!(
            fish_quoted.starts_with('\''),
            "fish_quote must start with '; got: {fish_quoted}"
        );
        let fish_glue = format!("set -gx LG_CONFIG_FILE {}; exec lazygit", fish_quoted);
        let after_eq = fish_glue
            .strip_prefix("set -gx LG_CONFIG_FILE ")
            .expect("prefix");
        let token_end = after_eq.find("; exec").expect("semicolon");
        let token = &after_eq[..token_end];
        assert!(
            !token.contains(' ') || token.starts_with('\''),
            "fish path token is properly quoted; got token: {token}"
        );
    }

    /// The dashboard tab's `cwd` comes from the default worktree's path, so a
    /// hostile path must be KDL-escaped rather than appearing raw (which would
    /// let `"` terminate the string early and inject KDL nodes).
    #[test]
    fn write_multi_tab_layout_hostile_dashboard_cwd() {
        let _guard = env_guard();
        // A basename that tries to break out of the KDL cwd= string. No `/` so
        // Path::file_name() returns the whole string. The parent dir adds a
        // space and a single-quote to exercise the rest of the path.
        let hostile_basename = "feat\" focus=true cwd=\"injected";
        let hostile_path = format!("/home/o'brien/my projects/{hostile_basename}");
        // Single worktree → session_cwd falls back to it, so it becomes the
        // dashboard cwd.
        let worktrees = vec![make_wt(&hostile_path, "some-branch")];
        let cfg = dummy_cfg();
        let layout_path =
            write_multi_tab_layout(&worktrees, "test", &cfg, &dummy_git_dir()).unwrap();
        let content = std::fs::read_to_string(&layout_path).unwrap();
        let _ = std::fs::remove_file(&layout_path);

        // The raw injection string must not appear verbatim in the KDL output.
        assert!(
            !content.contains("feat\" focus=true cwd=\"injected"),
            "raw KDL injection sequence must not appear in output; got:\n{content}"
        );
        // The escaped form of the hostile path should appear in the cwd (\").
        assert!(
            content.contains(r#"feat\" focus=true cwd=\"injected"#),
            "escaped hostile path should appear in dashboard cwd; got:\n{content}"
        );
        // The cwd attribute must not contain the raw hostile path verbatim.
        assert!(
            !content.contains(&format!("cwd=\"{hostile_path}\"")),
            "cwd must be KDL-escaped, not raw; got:\n{content}"
        );
    }
}
