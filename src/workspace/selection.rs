//! Best-effort per-session navigation state. This file never controls focus.
use super::DestinationKey;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedSelection {
    pub destination: Option<DestinationKey>,
    pub repository: Option<String>,
    pub agents: bool,
    pub collapsed: BTreeSet<String>,
    pub query: String,
    pub filter: String,
}

pub fn path_for(base: &Path, session: &str) -> Result<PathBuf> {
    let dir = base.join("ui");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let metadata = std::fs::symlink_metadata(&dir)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::getuid() }
        || metadata.mode() & 0o077 != 0
    {
        bail!("unsafe workspace selection directory {}", dir.display());
    }
    // Hex encoding is collision-free and cannot introduce a path separator.
    let name: String = session
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(dir.join(format!("{name}.json")))
}

pub fn read(path: &Path) -> Result<Option<SharedSelection>> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    if metadata.len() > 64 * 1024 || metadata.uid() != unsafe { libc::getuid() } {
        bail!("invalid workspace selection record");
    }
    serde_json::from_reader(file)
        .context("read workspace selection")
        .map(Some)
}

pub fn write(path: &Path, state: &SharedSelection) -> Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_extension(format!("{}-{n}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(state)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sessions_and_foreign_repo_keys_round_trip() {
        let base = std::env::temp_dir().join(format!("swamp-ui-{}", std::process::id()));
        let a = path_for(&base, "repo/session").unwrap();
        let b = path_for(&base, "repo-session").unwrap();
        assert_ne!(a, b);
        let state = SharedSelection {
            destination: Some(DestinationKey {
                repo: "foreign".into(),
                path: "/other/work1".into(),
                branch: "fix".into(),
                session: Some("fix".into()),
                subagent: None,
            }),
            ..Default::default()
        };
        write(&a, &state).unwrap();
        assert_eq!(read(&a).unwrap(), Some(state));
        assert_eq!(read(&b).unwrap(), None);
        std::fs::write(&a, "bad json").unwrap();
        assert!(read(&a).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
