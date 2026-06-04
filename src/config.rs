use anyhow::{Context, Result};
use std::path::PathBuf;

const LAZYGIT_CONFIG: &str = include_str!("config/lazygit.yml");

/// Resolved paths to the swamp-managed config files.
pub struct ConfigPaths {
    pub lazygit: PathBuf,
}

/// Returns the `$XDG_CONFIG_HOME/swamp` directory (falls back to `~/.config/swamp`).
fn swamp_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("swamp")
}

/// Write `content` to `path` only if the file is absent or differs.
/// Returns `true` if the file was (re)written.
fn write_if_changed(path: &PathBuf, content: &str) -> Result<bool> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == content {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    std::fs::write(path, content)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Ensure the embedded swamp configs are present on disk.
/// Writes only when absent or when the content differs (idempotent).
pub fn ensure_configs() -> Result<ConfigPaths> {
    let dir = swamp_config_dir();
    let lazygit = dir.join("lazygit.yml");

    write_if_changed(&lazygit, LAZYGIT_CONFIG)?;

    Ok(ConfigPaths { lazygit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("swamp-test-{}", std::process::id()));
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
        // Point XDG_CONFIG_HOME at a temp dir so we don't pollute the real one.
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        let paths = ensure_configs().unwrap();
        assert_eq!(paths.lazygit, base.join("swamp").join("lazygit.yml"));
        assert!(paths.lazygit.exists());
    }

    #[test]
    fn ensure_configs_idempotent() {
        let base = tmp_dir();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &base) };
        ensure_configs().unwrap();
        // Second call must not error and paths must still exist.
        let paths = ensure_configs().unwrap();
        assert!(paths.lazygit.exists());
    }
}
