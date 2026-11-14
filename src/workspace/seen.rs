//! Revision-specific, process-shared acknowledgement state for completed agents.
//!
//! The ledger is deliberately separate from daemon status: a malformed ledger
//! can hide no live state, and a new completion revision is never cleared by an
//! acknowledgement of an older revision.

use crate::daemon::state::{AgentStatus, WorktreeRow};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum acknowledgements retained in the shared ledger.
const MAX_ENTRIES: usize = 512;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn ensure_private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("seen ledger path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let meta =
        fs::symlink_metadata(parent).with_context(|| format!("stat {}", parent.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } {
        bail!(
            "seen ledger directory {} is not a user-owned real directory",
            parent.display()
        );
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("secure {}", parent.display()))?;
    }
    Ok(())
}

fn verify_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("seen ledger path has no parent")?;
    let meta = match fs::symlink_metadata(parent) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("stat {}", parent.display())),
    };
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } {
        bail!(
            "seen ledger directory {} is not a user-owned real directory",
            parent.display()
        );
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("secure {}", parent.display()))?;
    }
    Ok(())
}

fn read_ledger_file(path: &Path) -> Result<Option<Vec<u8>>> {
    verify_parent(path)?;
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
    };
    let meta = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } {
        bail!(
            "seen ledger {} is not a user-owned regular file",
            path.display()
        );
    }
    if meta.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure {}", path.display()))?;
    }
    let mut bytes = Vec::new();
    use std::io::Read;
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(Some(bytes))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SeenEntry {
    repo_id: String,
    branch: String,
    activity_id: String,
    completion_id: String,
    seen_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default)]
    entries: Vec<SeenEntry>,
}

/// A loaded, immutable view of the seen ledger. Reload at a low frequency
/// (for example, every 500 ms) rather than doing file IO while rendering rows.
#[derive(Debug, Clone, Default)]
pub struct SeenState {
    entries: Vec<SeenEntry>,
}

/// Location and writer for the process-shared seen ledger.
#[derive(Debug, Clone)]
pub struct SeenLedger {
    path: PathBuf,
}

/// Semantic state used by the workspace presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticState {
    Waiting,
    Working,
    Done,
    Idle,
    Unknown,
}

impl SemanticState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Working => "Working",
            Self::Done => "Done",
            Self::Idle => "Idle",
            Self::Unknown => "Unknown",
        }
    }
}

impl SeenLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The XDG state path, falling back to `~/.local/state/swamp/seen.json`.
    pub fn default_path() -> Result<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .context("cannot determine state directory: set XDG_STATE_HOME or HOME")?;
        Ok(base.join("swamp").join("seen.json"))
    }

    pub fn open_default() -> Result<Self> {
        Ok(Self::new(Self::default_path()?))
    }

    /// Read a complete snapshot. Atomic replacement ensures readers see either
    /// the previous valid JSON or the next valid JSON, never a partial write.
    pub fn load(&self) -> Result<SeenState> {
        let Some(bytes) = read_ledger_file(&self.path)? else {
            return Ok(SeenState::default());
        };
        let file: LedgerFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse seen ledger {}", self.path.display()))?;
        Ok(SeenState {
            entries: file.entries,
        })
    }

    /// Acknowledge only the exact completion revision captured by a visit.
    /// The row's branch and activity identity make the key survive slot moves.
    pub fn acknowledge(&self, repo_id: &str, row: &WorktreeRow) -> Result<()> {
        let (Some(activity_id), Some(completion_id)) =
            (row.activity_id.as_deref(), row.completion_id.as_deref())
        else {
            return Ok(());
        };
        if repo_id.is_empty() || row.branch.is_empty() {
            bail!("cannot acknowledge a completion without repository and branch identity");
        }
        ensure_private_parent(&self.path)?;
        let parent = self
            .path
            .parent()
            .context("seen ledger path has no parent")?;
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)
            .with_context(|| format!("open seen-ledger lock {}", lock_path.display()))?;
        let lock_meta = lock.metadata()?;
        if !lock_meta.is_file() || lock_meta.uid() != unsafe { libc::getuid() } {
            bail!(
                "seen-ledger lock {} is not a user-owned regular file",
                lock_path.display()
            );
        }
        if lock_meta.permissions().mode() & 0o777 != 0o600 {
            lock.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        lock.lock()
            .with_context(|| format!("lock {}", lock_path.display()))?;

        // Reload under the stable lock, then merge. A malformed existing file
        // is an error; never replace it with a partial view from one process.
        let mut ledger = match read_ledger_file(&self.path)? {
            Some(bytes) => serde_json::from_slice::<LedgerFile>(&bytes)
                .with_context(|| format!("parse seen ledger {}", self.path.display()))?,
            None => LedgerFile::default(),
        };
        let entry = SeenEntry {
            repo_id: repo_id.to_owned(),
            branch: row.branch.clone(),
            activity_id: activity_id.to_owned(),
            completion_id: completion_id.to_owned(),
            seen_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        // Exact-revision deduplication avoids losing a newer acknowledgement
        // when a delayed visit acknowledges an older captured activity. The
        // global size cap below bounds obsolete entries without ordering IDs.
        ledger.entries.retain(|old| {
            !(old.repo_id == entry.repo_id
                && old.branch == entry.branch
                && old.activity_id == entry.activity_id
                && old.completion_id == entry.completion_id)
        });
        ledger.entries.push(entry);
        ledger.entries.sort_by_key(|e| e.seen_at);
        if ledger.entries.len() > MAX_ENTRIES {
            ledger.entries.drain(..ledger.entries.len() - MAX_ENTRIES);
        }

        let bytes = serde_json::to_vec_pretty(&ledger)?;
        let tmp = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        use std::io::Write;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("replace seen ledger {}", self.path.display()))?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

impl SeenState {
    pub fn is_seen(&self, repo_id: &str, row: &WorktreeRow) -> bool {
        let (Some(activity_id), Some(completion_id)) =
            (row.activity_id.as_deref(), row.completion_id.as_deref())
        else {
            return false;
        };
        self.entries.iter().any(|entry| {
            entry.repo_id == repo_id
                && entry.branch == row.branch
                && entry.activity_id == activity_id
                && entry.completion_id == completion_id
        })
    }

    pub fn derive(&self, repo_id: &str, row: &WorktreeRow) -> SemanticState {
        // `agent_ts == 0` is the legacy absence signal. A default Idle enum is
        // not proof that a daemon has reported a completed or idle agent.
        if row.agent_ts == 0 {
            return SemanticState::Unknown;
        }
        match row.agent {
            AgentStatus::Waiting => SemanticState::Waiting,
            AgentStatus::Working => SemanticState::Working,
            AgentStatus::Idle if row.completion_id.is_some() && !self.is_seen(repo_id, row) => {
                SemanticState::Done
            }
            AgentStatus::Idle => SemanticState::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::AgentStatus;
    use crate::worktree::WorktreeKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_path() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "swamp-seen-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        dir.join("seen.json")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn row() -> WorktreeRow {
        WorktreeRow {
            name: "work1".into(),
            path: PathBuf::from("/repo/work1"),
            branch: "feature/a".into(),
            upstream: None,
            upstream_gone: false,
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflict: false,
            rebase: false,
            agent: AgentStatus::Idle,
            agent_ts: 2,
            activity_id: Some("activity-1".into()),
            completion_id: Some("complete-1".into()),
            session_name: None,
            head_ts: 0,
            harness: None,
            is_default: false,
            removal_block: None,
            deleting: false,
            subagents: Vec::new(),
            kind: WorktreeKind::Slot,
            slot_state: None,
            pin: None,
            title: String::new(),
            title_source: None,
            head_subject: None,
            openspec_change: None,
        }
    }

    #[test]
    fn seen_acknowledges_only_exact_revision_and_survives_slot_move() {
        let path = temp_path();
        let ledger = SeenLedger::new(&path);
        let original = row();
        assert_eq!(
            ledger.load().unwrap().derive("repo", &original),
            SemanticState::Done
        );
        ledger.acknowledge("repo", &original).unwrap();
        let seen = ledger.load().unwrap();
        assert!(seen.is_seen("repo", &original));
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.with_extension("lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut moved = original.clone();
        moved.name = "work3".into();
        moved.path = PathBuf::from("/repo/work3");
        assert_eq!(seen.derive("repo", &moved), SemanticState::Idle);
        moved.completion_id = Some("complete-2".into());
        assert_eq!(seen.derive("repo", &moved), SemanticState::Done);
        cleanup(&path);
    }

    #[test]
    fn semantic_state_uses_reported_status_and_unknown_legacy_signal() {
        let seen = SeenState::default();
        let mut r = row();
        r.agent = AgentStatus::Waiting;
        assert_eq!(seen.derive("repo", &r), SemanticState::Waiting);
        r.agent = AgentStatus::Working;
        assert_eq!(seen.derive("repo", &r), SemanticState::Working);
        r.agent = AgentStatus::Idle;
        r.completion_id = None;
        assert_eq!(seen.derive("repo", &r), SemanticState::Idle);
        r.agent_ts = 0;
        assert_eq!(seen.derive("repo", &r), SemanticState::Unknown);
    }

    #[test]
    fn acknowledging_captured_revision_does_not_clear_a_later_completion() {
        let path = temp_path();
        let ledger = SeenLedger::new(&path);
        let captured = row();
        let mut latest = captured.clone();
        latest.activity_id = Some("9999999999-1-99".into());
        latest.completion_id = Some("complete-2".into());
        ledger.acknowledge("repo", &captured).unwrap();
        assert_eq!(
            ledger.load().unwrap().derive("repo", &latest),
            SemanticState::Done
        );
        ledger.acknowledge("repo", &latest).unwrap();
        assert_eq!(
            ledger.load().unwrap().derive("repo", &latest),
            SemanticState::Idle
        );
        // A navigation started earlier may finish later. Its old captured
        // revision must not remove the newer acknowledgement.
        ledger.acknowledge("repo", &captured).unwrap();
        let state = ledger.load().unwrap();
        assert_eq!(state.derive("repo", &latest), SemanticState::Idle);
        assert_eq!(state.entries.len(), 2);
        cleanup(&path);
    }

    #[test]
    fn daemon_completion_snapshot_can_be_marked_seen() {
        let path = temp_path();
        let ledger = SeenLedger::new(&path);
        let mut daemon = crate::daemon::state::DaemonState::default();
        daemon.rows.insert("work1".into(), row());
        daemon.apply_hook("work1", "working", None, None).unwrap();
        let active_id = daemon.agents["work1"].activity_id.clone();
        daemon.apply_hook("work1", "waiting", None, None).unwrap();
        assert_eq!(daemon.agents["work1"].activity_id, active_id);
        daemon.apply_hook("work1", "working", None, None).unwrap();
        assert_eq!(daemon.agents["work1"].activity_id, active_id);
        daemon.apply_hook("work1", "idle", None, None).unwrap();
        let snapshot = daemon.snapshot();
        let completed = &snapshot.rows[0];
        assert!(completed.completion_id.is_some());
        assert_eq!(
            ledger.load().unwrap().derive("repo", completed),
            SemanticState::Done
        );
        ledger.acknowledge("repo", completed).unwrap();
        assert_eq!(
            ledger.load().unwrap().derive("repo", completed),
            SemanticState::Idle
        );
        cleanup(&path);
    }

    #[test]
    fn malformed_ledger_is_an_error_not_an_empty_success() {
        let path = temp_path();
        fs::write(&path, b"{").unwrap();
        assert!(SeenLedger::new(&path).load().is_err());
        assert!(SeenLedger::new(&path).acknowledge("repo", &row()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{");
        cleanup(&path);
    }

    #[test]
    fn ledger_symlink_is_rejected_without_touching_its_target() {
        use std::os::unix::fs::symlink;
        let path = temp_path();
        let target = path.with_file_name("target.json");
        fs::write(&target, b"target data").unwrap();
        symlink(&target, &path).unwrap();
        let ledger = SeenLedger::new(&path);
        assert!(ledger.load().is_err());
        assert!(ledger.acknowledge("repo", &row()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"target data");
        cleanup(&path);
    }

    #[test]
    fn concurrent_writers_merge_and_retention_is_bounded() {
        let path = temp_path();
        let mut threads = Vec::new();
        for i in 0..8 {
            let p = path.clone();
            threads.push(std::thread::spawn(move || {
                let mut r = row();
                r.branch = format!("branch-{i}");
                r.activity_id = Some(format!("activity-{i}"));
                r.completion_id = Some(format!("complete-{i}"));
                SeenLedger::new(p).acknowledge("repo", &r).unwrap();
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        let ledger = SeenLedger::new(&path);
        let seen = ledger.load().unwrap();
        assert_eq!(
            seen.entries.len(),
            8,
            "writers must merge rather than replace"
        );
        for i in 8..(MAX_ENTRIES + 8) {
            let mut r = row();
            r.branch = format!("branch-{i}");
            r.activity_id = Some(format!("activity-{i}"));
            r.completion_id = Some(format!("complete-{i}"));
            ledger.acknowledge("repo", &r).unwrap();
        }
        assert_eq!(ledger.load().unwrap().entries.len(), MAX_ENTRIES);
        cleanup(&path);
    }
}
