use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::types::{DashboardConfig, HarnessSetting, SwampConfig};

const LAZYGIT_CONFIG: &str = include_str!("lazygit.yml");
const DEFAULT_CONFIG_TOML: &str = include_str!("config.toml");

/// Resolved paths to the swamp-managed config files, plus the loaded settings.
pub struct ConfigPaths {
    pub lazygit: PathBuf,
    pub dashboard: DashboardConfig,
    /// Repo-wide harness preference (per-worktree overrides apply in `choose`).
    pub harness: HarnessSetting,
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
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
        Err(_) => Ok(SwampConfig::default()),
    }
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let _guard = ENV_LOCK.lock().unwrap();
        // Point XDG_CONFIG_HOME at a temp dir so we don't pollute the real one.
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let paths = ensure_configs().unwrap();
        assert_eq!(paths.lazygit, base.join("swamp").join("lazygit.yml"));
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn ensure_configs_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        ensure_configs().unwrap();
        // Second call must not error and paths must still exist.
        let paths = ensure_configs().unwrap();
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn malformed_config_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let path = base.join("swamp").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[harness]\ndefault = [").unwrap();

        let err = load_config().unwrap_err();
        assert!(err.to_string().contains("parse"));
        assert!(err.to_string().contains("config.toml"));
    }
}
