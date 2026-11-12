//! The persistent list of repositories that swamp has launched.
//!
//! A daemon socket exists only while its daemon runs, so cross-repo features
//! need a record that survives `swamp kill`. `swamp launch` writes an entry,
//! `swamp register` adds one without a launch, and `swamp forget` removes one.

use crate::discover::{self, Probe};
use crate::util::{now_unix, session_name_for};
use crate::worktree::{git_common_dir, resolve_git_dir};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    /// Canonical git common directory. This is the key of the entry.
    pub common_dir: PathBuf,
    pub session_name: String,
    /// Unix time of the last launch or registration.
    #[serde(default)]
    pub last_launched: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    repos: Vec<RepoEntry>,
}

fn path_from(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let non_empty = |v: &OsString| !v.is_empty();
    xdg_state_home
        .filter(non_empty)
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(non_empty)
                .map(|h| PathBuf::from(h).join(".local").join("state"))
        })
        .map(|base| base.join("swamp").join("repos.json"))
}

/// `$XDG_STATE_HOME/swamp/repos.json`, or `$HOME/.local/state/swamp/repos.json`.
pub fn registry_path() -> Result<PathBuf> {
    path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
        .context("cannot locate the repo registry: neither XDG_STATE_HOME nor HOME is set")
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

enum Stored {
    Missing,
    Corrupt,
    Entries(Vec<RepoEntry>),
}

fn read_stored(path: &Path) -> Stored {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Stored::Missing,
        Err(e) => {
            tracing::warn!("could not read repo registry {}: {e}", path.display());
            return Stored::Corrupt;
        }
    };
    match serde_json::from_str::<RegistryFile>(&text) {
        Ok(file) => Stored::Entries(file.repos),
        Err(e) => {
            tracing::warn!("repo registry {} is malformed: {e}", path.display());
            Stored::Corrupt
        }
    }
}

/// Drop entries whose repository no longer exists on disk.
fn existing(entries: Vec<RepoEntry>) -> Vec<RepoEntry> {
    entries
        .into_iter()
        .filter(|e| e.common_dir.exists())
        .collect()
}

/// Read the registry at `path`. A missing or malformed file reads as empty.
pub fn load_at(path: &Path) -> Vec<RepoEntry> {
    match read_stored(path) {
        Stored::Entries(entries) => existing(entries),
        Stored::Missing | Stored::Corrupt => Vec::new(),
    }
}

/// Apply `change` to the registry at `path` under an exclusive lock, then
/// replace the file atomically. Entries for deleted repositories are pruned,
/// and a malformed file is set aside as `repos.json.corrupt`.
fn mutate_at<T>(path: &Path, change: impl FnOnce(&mut Vec<RepoEntry>) -> T) -> Result<T> {
    let dir = path.parent().context("registry path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let _lock = crate::daemon::flock_exclusive(&path.with_extension("json.lock"))?;

    let mut entries = match read_stored(path) {
        Stored::Entries(entries) => existing(entries),
        Stored::Missing => Vec::new(),
        Stored::Corrupt => {
            let quarantine = path.with_extension("json.corrupt");
            match std::fs::rename(path, &quarantine) {
                Ok(()) => {
                    tracing::warn!("moved malformed repo registry to {}", quarantine.display())
                }
                Err(e) => tracing::warn!("could not set aside malformed repo registry: {e}"),
            }
            Vec::new()
        }
    };
    let out = change(&mut entries);

    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let body = serde_json::to_vec_pretty(&RegistryFile { repos: entries })?;
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(out)
}

/// Add the repository, or refresh its entry. Returns the stored entry.
pub fn upsert_at(
    path: &Path,
    common_dir: &Path,
    session_name: &str,
    now: u64,
) -> Result<RepoEntry> {
    let entry = RepoEntry {
        common_dir: canonical(common_dir),
        session_name: session_name.to_string(),
        last_launched: now,
    };
    mutate_at(path, |entries| {
        entries.retain(|e| e.common_dir != entry.common_dir);
        entries.push(entry.clone());
    })?;
    Ok(entry)
}

/// Remove the repository. Returns whether an entry existed.
pub fn remove_at(path: &Path, common_dir: &Path) -> Result<bool> {
    let key = canonical(common_dir);
    mutate_at(path, |entries| {
        let before = entries.len();
        entries.retain(|e| e.common_dir != key);
        entries.len() != before
    })
}

/// Record a launch of the repository. A registry fault must not block a
/// launch, so a failure is logged and otherwise ignored.
pub fn record_launch(common_dir: &Path) {
    let result = registry_path()
        .and_then(|path| upsert_at(&path, common_dir, &session_name_for(common_dir), now_unix()));
    if let Err(e) = result {
        tracing::warn!("could not record repo in the registry: {e:#}");
    }
}

fn common_dir_for(dir: Option<PathBuf>) -> Result<PathBuf> {
    let start = match dir {
        Some(p) => {
            std::fs::canonicalize(&p).with_context(|| format!("canonicalize {}", p.display()))?
        }
        None => std::env::current_dir()?,
    };
    git_common_dir(&resolve_git_dir(&start)).context("not inside a git repo")
}

/// Result of `swamp register --all-live`.
#[derive(Debug, Default)]
pub struct AllLiveReport {
    pub registered: Vec<RepoEntry>,
    /// Sockets of live daemons whose repository could not be determined.
    pub skipped: Vec<PathBuf>,
}

/// Register the repository of each live daemon in `runtime_dir`. This only
/// reads from the daemons: none is started, stopped, or restarted.
pub async fn register_all_live_at(registry: &Path, runtime_dir: &Path) -> Result<AllLiveReport> {
    let mut report = AllLiveReport::default();
    let known = load_at(registry);
    for sock in discover::socket_files(runtime_dir) {
        match discover::probe(&sock, &known).await {
            Probe::NotLive => {}
            Probe::Unidentified => report.skipped.push(sock),
            Probe::Identified(id) => {
                let entry = upsert_at(registry, &id.common_dir, &id.session_name, now_unix())?;
                report.registered.push(entry);
            }
        }
    }
    Ok(report)
}

pub async fn run_register(dir: Option<PathBuf>, all_live: bool) -> Result<()> {
    let registry = registry_path()?;
    if !all_live {
        let common = common_dir_for(dir)?;
        let entry = upsert_at(&registry, &common, &session_name_for(&common), now_unix())?;
        println!(
            "registered {} ({})",
            entry.session_name,
            entry.common_dir.display()
        );
        return Ok(());
    }

    let runtime_dir = crate::util::runtime_base_dir()?;
    let report = register_all_live_at(&registry, &runtime_dir).await?;
    for entry in &report.registered {
        println!(
            "registered {} ({})",
            entry.session_name,
            entry.common_dir.display()
        );
    }
    for sock in &report.skipped {
        println!(
            "skipped {}: could not determine its repository",
            sock.display()
        );
    }
    if report.registered.is_empty() && report.skipped.is_empty() {
        println!("no live swamp daemons found");
    }
    Ok(())
}

pub fn run_forget(dir: Option<PathBuf>) -> Result<()> {
    let common = common_dir_for(dir)?;
    if remove_at(&registry_path()?, &common)? {
        println!("forgot {}", canonical(&common).display());
    } else {
        println!("no registry entry for {}", canonical(&common).display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::RepoIdentity;
    use crate::discover::test_support::{InfoReply, row, spawn_fake_daemon, temp_dir};
    use crate::util::repo_id;
    use crate::worktree::create_worktree;
    use crate::worktree::test_support::{git_available, setup};

    fn repo_dir(base: &Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_prefers_xdg_state_home_then_home() {
        assert_eq!(
            path_from(Some("/state".into()), Some("/home/u".into())),
            Some(PathBuf::from("/state/swamp/repos.json"))
        );
        assert_eq!(
            path_from(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.local/state/swamp/repos.json"))
        );
        assert_eq!(
            path_from(Some("".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.local/state/swamp/repos.json"))
        );
        assert_eq!(path_from(None, None), None);
    }

    #[test]
    fn upsert_keeps_one_entry_per_repo() {
        let tmp = temp_dir("reg-upsert");
        let reg = tmp.join("state").join("repos.json");
        let a = repo_dir(&tmp, "a");
        let b = repo_dir(&tmp, "b");

        upsert_at(&reg, &a, "a-1111", 10).unwrap();
        upsert_at(&reg, &b, "b-2222", 20).unwrap();
        upsert_at(&reg, &a, "a-1111", 30).unwrap();

        let mut entries = load_at(&reg);
        entries.sort_by(|x, y| x.session_name.cmp(&y.session_name));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].common_dir, a.canonicalize().unwrap());
        assert_eq!(entries[0].last_launched, 30);
        assert_eq!(entries[1].last_launched, 20);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn symlinked_path_resolves_to_the_same_entry() {
        let tmp = temp_dir("reg-symlink");
        let reg = tmp.join("repos.json");
        let real = repo_dir(&tmp, "real");
        let link = tmp.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        upsert_at(&reg, &real, "real-1111", 1).unwrap();
        upsert_at(&reg, &link, "real-1111", 2).unwrap();

        let entries = load_at(&reg);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].last_launched, 2);
        assert!(remove_at(&reg, &link).unwrap());
        assert!(load_at(&reg).is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deleted_repo_is_hidden_then_pruned() {
        let tmp = temp_dir("reg-stale");
        let reg = tmp.join("repos.json");
        let gone = repo_dir(&tmp, "gone");
        let kept = repo_dir(&tmp, "kept");
        upsert_at(&reg, &gone, "gone-1111", 1).unwrap();
        upsert_at(&reg, &kept, "kept-2222", 2).unwrap();

        std::fs::remove_dir_all(&gone).unwrap();
        // A reader hides the entry without a write.
        assert_eq!(load_at(&reg).len(), 1);
        assert!(std::fs::read_to_string(&reg).unwrap().contains("gone-1111"));
        // The next write removes it from the file.
        upsert_at(&reg, &kept, "kept-2222", 3).unwrap();
        assert!(!std::fs::read_to_string(&reg).unwrap().contains("gone-1111"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn remove_reports_whether_an_entry_existed() {
        let tmp = temp_dir("reg-remove");
        let reg = tmp.join("repos.json");
        let a = repo_dir(&tmp, "a");
        assert!(!remove_at(&reg, &a).unwrap());
        upsert_at(&reg, &a, "a-1111", 1).unwrap();
        assert!(remove_at(&reg, &a).unwrap());
        assert!(!remove_at(&reg, &a).unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn concurrent_writers_keep_every_entry() {
        let tmp = temp_dir("reg-concurrent");
        let reg = tmp.join("repos.json");
        let dirs: Vec<PathBuf> = (0..16)
            .map(|i| repo_dir(&tmp, &format!("repo{i}")))
            .collect();

        std::thread::scope(|scope| {
            for (i, dir) in dirs.iter().enumerate() {
                let reg = &reg;
                scope.spawn(move || {
                    upsert_at(reg, dir, &format!("repo{i}"), i as u64).unwrap();
                });
            }
        });

        assert_eq!(load_at(&reg).len(), dirs.len());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn malformed_file_is_set_aside_and_treated_as_empty() {
        let tmp = temp_dir("reg-corrupt");
        let reg = tmp.join("repos.json");
        std::fs::write(&reg, "{ not json").unwrap();
        let a = repo_dir(&tmp, "a");

        assert!(load_at(&reg).is_empty());
        upsert_at(&reg, &a, "a-1111", 1).unwrap();

        assert_eq!(load_at(&reg).len(), 1);
        assert_eq!(
            std::fs::read_to_string(tmp.join("repos.json.corrupt")).unwrap(),
            "{ not json"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unwritable_registry_is_an_error_that_the_launch_path_ignores() {
        let tmp = temp_dir("reg-unwritable");
        // The registry directory cannot be created under a regular file.
        let blocker = tmp.join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let reg = blocker.join("swamp").join("repos.json");
        let a = repo_dir(&tmp, "a");

        assert!(upsert_at(&reg, &a, "a-1111", 1).is_err());
        assert!(load_at(&reg).is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn register_all_live_registers_new_and_old_daemons_and_reports_skips() {
        if !git_available() {
            return;
        }
        let tmp = temp_dir("reg-all-live");
        let reg = tmp.join("state").join("repos.json");
        let runtime = repo_dir(&tmp, "runtime");

        // A current daemon, which answers `GetInfo`.
        let new_repo = repo_dir(&tmp, "new-repo");
        let new_sock = runtime.join("aaaaaaaaaaaaaaaa.sock");
        let new_daemon = spawn_fake_daemon(
            &new_sock,
            InfoReply::Answer(RepoIdentity {
                common_dir: new_repo.clone(),
                session_name: "new-repo-aaaa".into(),
                version: Some("9.9.9".into()),
            }),
            Vec::new(),
        );

        // An older daemon, identified from its worktree paths.
        let (old_root, old_bare) = setup();
        let wt = create_worktree(&old_bare, "feature").unwrap();
        let old_sock = runtime.join(format!("{}.sock", repo_id(&old_bare)));
        let old_daemon =
            spawn_fake_daemon(&old_sock, InfoReply::Reject, vec![row("feature", &wt.path)]);

        // An older daemon that cannot be identified, and a stale socket file.
        let lost_sock = runtime.join("cccccccccccccccc.sock");
        let lost_daemon = spawn_fake_daemon(
            &lost_sock,
            InfoReply::Reject,
            vec![row("gone", &tmp.join("no-such-worktree"))],
        );
        let stale_sock = runtime.join("dddddddddddddddd.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale_sock).unwrap());

        let report = register_all_live_at(&reg, &runtime).await.unwrap();

        let mut sessions: Vec<&str> = report
            .registered
            .iter()
            .map(|e| e.session_name.as_str())
            .collect();
        sessions.sort();
        let old_session = session_name_for(&old_bare);
        let mut expected = vec!["new-repo-aaaa", old_session.as_str()];
        expected.sort();
        assert_eq!(sessions, expected);
        assert_eq!(report.skipped, vec![lost_sock.clone()]);
        assert_eq!(load_at(&reg).len(), 2);

        // Registration only reads: each daemon still accepts connections.
        for sock in [&new_sock, &old_sock, &lost_sock] {
            assert!(tokio::net::UnixStream::connect(sock).await.is_ok());
        }

        new_daemon.abort();
        old_daemon.abort();
        lost_daemon.abort();
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&old_root);
    }
}
