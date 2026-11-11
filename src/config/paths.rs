use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::types::{
    DashboardConfig, HarnessSetting, LoggingConfig, RepoConfig, SlotsConfig, SwampConfig,
};

const LAZYGIT_CONFIG: &str = include_str!("lazygit.yml");
const DEFAULT_CONFIG_TOML: &str = include_str!("config.toml");

/// Resolved paths to the swamp-managed config files, plus the loaded settings.
pub struct ConfigPaths {
    pub lazygit: PathBuf,
    pub dashboard: DashboardConfig,
    /// Repo-wide harness preference (per-worktree overrides apply in `choose`).
    pub harness: HarnessSetting,
    /// Diagnostic-logging settings, used to initialize the tracing subscriber.
    pub logging: LoggingConfig,
}

/// Returns the `$XDG_CONFIG_HOME/swamp` directory (falls back to
/// `$HOME/.config/swamp`).
fn swamp_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("swamp")
}

/// Path to the user's TOML config.
fn config_toml_path() -> PathBuf {
    swamp_config_dir().join("config.toml")
}

/// Load the user's `config.toml`. A missing file yields defaults; a malformed
/// one aborts so typos do not silently change launch behavior.
pub fn load_config() -> Result<SwampConfig> {
    let path = config_toml_path();
    let cfg: SwampConfig = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?,
        Err(_) => SwampConfig::default(),
    };
    // A bad `[logging] filter` directive is rejected here, alongside other
    // malformed config, rather than being silently dropped at subscriber init.
    if let Some(filter) = &cfg.logging.filter {
        tracing_subscriber::EnvFilter::try_new(filter)
            .with_context(|| format!("invalid [logging] filter in {}", path.display()))?;
    }
    Ok(cfg)
}

/// Path to the per-repository config under the git common dir.
pub fn repo_config_path(common_dir: &Path) -> PathBuf {
    common_dir.join("swamp").join("config.toml")
}

/// Load the per-repository config. A missing file yields defaults; a malformed
/// one is an error, the same as the user config.
pub fn load_repo_config(common_dir: &Path) -> Result<RepoConfig> {
    let path = repo_config_path(common_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
        Err(_) => Ok(RepoConfig::default()),
    }
}

/// The effective slot settings for one repository: the user config with the
/// per-repository config applied on top.
pub fn slots_config(common_dir: &Path) -> Result<SlotsConfig> {
    let user = load_config()?.slots;
    let repo = load_repo_config(common_dir)?;
    Ok(user.merged_with(&repo.slots))
}

/// Write the default `config.toml` if it doesn't exist yet. Unlike the embedded
/// configs, this is user-owned: an existing file is never clobbered. Returns the
/// path and whether a new file was written.
pub fn ensure_config_toml() -> Result<(PathBuf, bool)> {
    let path = config_toml_path();
    if path.exists() {
        return Ok((path, false));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(&path, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("write {}", path.display()))?;
    Ok((path, true))
}

/// Write `content` to `path` only if the file is absent or differs.
/// Returns `true` if the file was (re)written.
fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == content
    {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Ensure the embedded swamp configs are present on disk and load user settings.
/// Writes managed files only when absent or differing (idempotent).
pub fn ensure_configs() -> Result<ConfigPaths> {
    let dir = swamp_config_dir();
    let lazygit = dir.join("lazygit.yml");

    write_if_changed(&lazygit, LAZYGIT_CONFIG)?;

    let cfg = load_config()?;
    Ok(ConfigPaths {
        lazygit,
        dashboard: cfg.dashboard,
        harness: cfg.harness.default,
        logging: cfg.logging,
    })
}

/// Whether `path` can't be written: a symlink into the immutable nix store
/// (home-manager) or a file whose permissions are read-only.
pub(super) fn is_read_only(path: &Path) -> bool {
    if let Ok(target) = std::fs::read_link(path)
        && target.starts_with("/nix/store")
    {
        return true;
    }
    std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_dir() -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("swamp-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn write_if_changed_creates_file() {
        let dir = tmp_dir();
        let path = dir.join("sub").join("file.toml");
        let wrote = write_if_changed(&path, "hello").unwrap();
        assert!(wrote, "should have written a new file");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn write_if_changed_idempotent() {
        let dir = tmp_dir();
        let path = dir.join("idempotent.toml");
        write_if_changed(&path, "content").unwrap();
        let wrote = write_if_changed(&path, "content").unwrap();
        assert!(!wrote, "should NOT rewrite when content matches");
    }

    #[test]
    fn write_if_changed_overwrites_on_mismatch() {
        let dir = tmp_dir();
        let path = dir.join("mismatch.toml");
        write_if_changed(&path, "old").unwrap();
        let wrote = write_if_changed(&path, "new").unwrap();
        assert!(wrote, "should rewrite when content differs");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn ensure_configs_returns_expected_paths() {
        let _guard = crate::util::test_env_lock();
        // Point XDG_CONFIG_HOME at a temp dir so we don't pollute the real one.
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let paths = ensure_configs().unwrap();
        assert_eq!(paths.lazygit, base.join("swamp").join("lazygit.yml"));
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn ensure_configs_idempotent() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        ensure_configs().unwrap();
        // Second call must not error and paths must still exist.
        let paths = ensure_configs().unwrap();
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn malformed_config_errors() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let path = base.join("swamp").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[harness]\ndefault = [").unwrap();

        let err = load_config().unwrap_err();
        assert!(err.to_string().contains("parse"));
        assert!(err.to_string().contains("config.toml"));
    }

    #[test]
    fn invalid_logging_filter_errors() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let path = base.join("swamp").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A directive with an invalid level is rejected by EnvFilter parsing.
        fs::write(&path, "[logging]\nfilter = \"swamp=notalevel\"\n").unwrap();

        let err = load_config().unwrap_err();
        assert!(err.to_string().contains("config.toml"));
    }

    #[test]
    fn slots_pool_size_defaults_to_five() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let common = base.join("repo.git");
        fs::create_dir_all(&common).unwrap();

        assert_eq!(slots_config(&common).unwrap().pool_size, 5);
    }

    #[test]
    fn repo_config_overrides_user_pool_size() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let user = base.join("swamp").join("config.toml");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(&user, "[slots]\npool_size = 5\n").unwrap();
        let common = base.join("repo.git");
        fs::create_dir_all(common.join("swamp")).unwrap();
        fs::write(repo_config_path(&common), "[slots]\npool_size = 2\n").unwrap();

        assert_eq!(slots_config(&common).unwrap().pool_size, 2);
    }

    #[test]
    fn pool_size_zero_is_allowed() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let user = base.join("swamp").join("config.toml");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(&user, "[slots]\npool_size = 0\n").unwrap();
        let common = base.join("repo.git");
        fs::create_dir_all(&common).unwrap();

        assert_eq!(slots_config(&common).unwrap().pool_size, 0);
    }

    #[test]
    fn malformed_repo_config_errors() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let common = base.join("repo.git");
        fs::create_dir_all(common.join("swamp")).unwrap();
        fs::write(repo_config_path(&common), "[slots]\npool_size = \"x\"\n").unwrap();

        let err = slots_config(&common).unwrap_err();
        assert!(err.to_string().contains("parse"), "{err}");
    }

    #[test]
    fn slot_hooks_run_user_first_then_repo() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let user = base.join("swamp").join("config.toml");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(
            &user,
            "[[slots.hooks.pre-release]]\nrun = \"bazel shutdown\"\n",
        )
        .unwrap();
        let common = base.join("repo.git");
        fs::create_dir_all(common.join("swamp")).unwrap();
        fs::write(
            repo_config_path(&common),
            "[[slots.hooks.pre-release]]\nrun = \"./cleanup.sh\"\ntimeout_secs = 10\n\n[[slots.hooks.post-alloc]]\nrun = \"direnv allow\"\n",
        )
        .unwrap();

        let hooks = slots_config(&common).unwrap().hooks;

        let pre: Vec<_> = hooks.pre_release.iter().map(|h| h.run.as_str()).collect();
        assert_eq!(pre, ["bazel shutdown", "./cleanup.sh"]);
        assert_eq!(hooks.pre_release[1].timeout_secs, Some(10));
        assert_eq!(hooks.post_alloc[0].run, "direnv allow");
    }

    #[test]
    fn unknown_hook_event_is_rejected() {
        let _guard = crate::util::test_env_lock();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let user = base.join("swamp").join("config.toml");
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(&user, "[[slots.hooks.post-release]]\nrun = \"x\"\n").unwrap();

        assert!(load_config().is_err());
    }
}
