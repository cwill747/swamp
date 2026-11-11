//! The slot pool: reusable worktrees named `work1`..`work<P>`.
//!
//! Git state is the only record of slot ownership. A slot is free when its
//! `HEAD` is detached, its tree is clean, and no pin file exists for it. Alloc
//! checks a branch out into the lowest free slot (creating a slot lazily, up to
//! the pool size) or, when the pool is full, into a dedicated branch-named
//! overflow worktree. Release detaches the slot at the branch tip and keeps the
//! directory, so ignored build output stays warm for the next branch.
//!
//! Every mutation holds an exclusive lock on `<common>/swamp/slots.lock`, which
//! the CLI and the daemon share.

use super::branches::{default_branch_name, default_remote, list_worktrees};
use super::create::{
    add_worktree_named, base_remote, branch_remote, find_remote_tracking_branch, inflate_lfs,
    workon_root,
};
use super::model::{
    GitInfo, RemovalVerdict, RemoveRefusedReason, SlotState, VerdictContext, Worktree,
    WorktreeKind, worktree_name_for_branch,
};
use super::remove::branch_safety;
use super::status::git_info;
use anyhow::{Context, Result, bail};
use git2::build::CheckoutBuilder;
use git2::{BranchType, Oid, Repository, RepositoryState};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SLOT_PREFIX: &str = "work";

/// The slot number `N` of a worktree named `work<N>`, for `N` of 1 or more with
/// no leading zero. Any such name is a slot, whatever the pool size, so that a
/// pool size change never turns a slot into an overflow worktree.
pub fn slot_number(name: &str) -> Option<u32> {
    let digits = name.strip_prefix(SLOT_PREFIX)?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn slot_name(n: u32) -> String {
    format!("{SLOT_PREFIX}{n}")
}

/// Directory name for an overflow worktree: the sanitized branch, with `-1`
/// added when that name would read as a slot.
pub fn overflow_name_for_branch(branch: &str) -> String {
    let name = worktree_name_for_branch(branch);
    if slot_number(&name).is_some() {
        format!("{name}-1")
    } else {
        name
    }
}

/// The role of the worktree named `name` with `branch` checked out.
pub fn worktree_kind(name: &str, branch: &str, default_branch: &str) -> WorktreeKind {
    if slot_number(name).is_some() {
        WorktreeKind::Slot
    } else if !default_branch.is_empty() && branch == default_branch {
        WorktreeKind::Default
    } else {
        WorktreeKind::Overflow
    }
}

/// Turn a requested name into a valid branch name: every character outside
/// `[A-Za-z0-9._/-]` becomes `-`, leading `-` and `/` are dropped, and runs of
/// `/` collapse to one.
pub fn sanitize_branch_name(requested: &str) -> Result<String> {
    let mut out = String::with_capacity(requested.len());
    for c in requested.trim().chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-') {
            c
        } else {
            '-'
        };
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    let out = out
        .trim_start_matches(['-', '/'])
        .trim_end_matches('/')
        .to_string();
    if out.is_empty() || !git2::Branch::name_is_valid(&out).unwrap_or(false) {
        bail!("'{requested}' is not a usable branch name");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Sidecar state: pins and the lock live under `<common>/swamp/`, outside every
// worktree, so they never show in `git status`.

fn swamp_dir(common: &Path) -> PathBuf {
    common.join("swamp")
}

fn pin_path(common: &Path, slot: &str) -> PathBuf {
    swamp_dir(common).join("pins").join(slot)
}

/// The pin reason for `slot`, if a pin exists.
pub fn read_pin(common: &Path, slot: &str) -> Option<String> {
    std::fs::read_to_string(pin_path(common, slot))
        .ok()
        .map(|s| s.trim().to_string())
}

fn clear_pin(common: &Path, slot: &str) -> bool {
    std::fs::remove_file(pin_path(common, slot)).is_ok()
}

/// An exclusive lock that serializes slot operations across processes. The
/// lock is released when the value drops.
pub struct SlotLock {
    _file: File,
}

pub fn lock(common: &Path) -> Result<SlotLock> {
    let dir = swamp_dir(common);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("slots.lock");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    file.lock()
        .with_context(|| format!("lock {}", path.display()))?;
    Ok(SlotLock { _file: file })
}

// ---------------------------------------------------------------------------
// Classification

/// Classify the slot at `path` from git and pin state. Any read error counts as
/// busy, so swamp never hands out a slot it cannot inspect.
pub fn classify(common: &Path, name: &str, path: &Path) -> (SlotState, Option<String>) {
    let pin = read_pin(common, name);
    let Ok(repo) = Repository::open(path) else {
        return (SlotState::Busy, pin);
    };
    if !repo.head_detached().unwrap_or(false) || repo.state() != RepositoryState::Clean {
        return (SlotState::Busy, pin);
    }
    match git_info(path) {
        Ok(info) => slot_state_from(true, &info, pin),
        Err(_) => (SlotState::Busy, pin),
    }
}

/// Slot state from data a status scan already has: whether `HEAD` is
/// detached, the worktree's git status, and its pin.
pub fn slot_state_from(
    detached: bool,
    info: &GitInfo,
    pin: Option<String>,
) -> (SlotState, Option<String>) {
    if !detached || info.is_dirty() || info.rebase {
        return (SlotState::Busy, pin);
    }
    match pin {
        Some(reason) => (SlotState::Pinned, Some(reason)),
        None => (SlotState::Free, None),
    }
}

/// The checked-out branch of the worktree at `path`, or `None` when `HEAD` is
/// detached or unreadable.
pub fn checked_out_branch(path: &Path) -> Option<String> {
    let repo = Repository::open(path).ok()?;
    if repo.head_detached().unwrap_or(true) {
        return None;
    }
    repo.head().ok()?.shorthand().ok().map(String::from)
}

/// One row of `swamp slot list`.
#[derive(Debug, Clone, Serialize)]
pub struct PoolEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: WorktreeKind,
    /// Slot state; `None` for the default and overflow worktrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<SlotState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// False for a slot whose number is larger than the pool size.
    pub in_pool: bool,
}

/// List every slot and overflow worktree, slots first in number order.
pub fn list_pool(common: &Path, pool_size: u32) -> Result<Vec<PoolEntry>> {
    let repo =
        Repository::open(common).with_context(|| format!("open repo at {}", common.display()))?;
    let default = default_branch_name(&repo).unwrap_or_default();
    let mut entries: Vec<PoolEntry> = list_worktrees(common)?
        .into_iter()
        .map(|wt| {
            let name = wt.name();
            let branch = checked_out_branch(&wt.path);
            let kind = worktree_kind(&name, branch.as_deref().unwrap_or(""), &default);
            let (state, pin, in_pool) = match slot_number(&name) {
                Some(n) => {
                    let (state, pin) = classify(common, &name, &wt.path);
                    (Some(state), pin, n <= pool_size)
                }
                None => (None, None, true),
            };
            PoolEntry {
                name,
                path: wt.path,
                kind,
                state,
                branch,
                pin,
                in_pool,
            }
        })
        .collect();
    entries.sort_by_key(|e| {
        (
            e.kind != WorktreeKind::Default,
            slot_number(&e.name).is_none(),
            slot_number(&e.name),
            e.name.clone(),
        )
    });
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Alloc

#[derive(Debug, Clone, Default)]
pub struct AllocOptions {
    pub pool_size: u32,
    /// Fetch the default remote before allocating (best effort).
    pub fetch: bool,
    /// Create the branch from this base (a branch, `origin/<base>`, tag, or
    /// SHA) instead of the default branch. The branch must not exist yet.
    pub base: Option<String>,
}

impl AllocOptions {
    pub fn pool(pool_size: u32) -> Self {
        Self {
            pool_size,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub name: String,
    pub path: PathBuf,
    pub kind: WorktreeKind,
    pub branch: String,
    /// True when a worktree already had the branch and nothing changed.
    pub existing: bool,
    /// True when swamp created the branch.
    pub created_branch: bool,
    /// Non-fatal problems (for example, a failed fetch) for the caller to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// `post-alloc` hook results, for the caller to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<crate::slot_hooks::HookRun>,
}

/// Give the caller a worktree for branch `requested`. See the module docs.
/// Production callers go through [`crate::slot_service::alloc`].
#[cfg(test)]
pub fn alloc(common: &Path, requested: &str, opts: &AllocOptions) -> Result<Allocation> {
    let guard = lock(common)?;
    alloc_locked(&guard, common, requested, opts)
}

/// [`alloc`] for a caller that already holds the slot lock.
pub fn alloc_locked(
    _lock: &SlotLock,
    common: &Path,
    requested: &str,
    opts: &AllocOptions,
) -> Result<Allocation> {
    let branch = sanitize_branch_name(requested)?;
    let mut warnings = Vec::new();

    let repo =
        Repository::open(common).with_context(|| format!("open repo at {}", common.display()))?;
    if opts.fetch
        && let Some(remote) = default_remote(&repo)
        && let Err(e) = git_subprocess(common, &["fetch", "--quiet", &remote])
    {
        warnings.push(format!("fetch {remote} failed ({e:#}); using cached refs"));
    }
    let default = default_branch_name(&repo).unwrap_or_default();
    let worktrees = list_worktrees(common)?;

    if let Some(wt) = worktrees
        .iter()
        .find(|w| checked_out_branch(&w.path).as_deref() == Some(branch.as_str()))
    {
        let name = wt.name();
        return Ok(Allocation {
            kind: worktree_kind(&name, &branch, &default),
            name,
            path: wt.path.clone(),
            branch,
            existing: true,
            created_branch: false,
            warnings,
            hooks: Vec::new(),
        });
    }

    let (remote, created_branch) = match opts.base.as_deref() {
        Some(base) => (create_branch_from_base(&repo, &branch, base)?, true),
        None => ensure_local_branch(&repo, &branch, &default)?,
    };
    match place(&repo, common, &worktrees, &branch, remote.as_deref(), opts) {
        Ok((name, path, kind)) => Ok(Allocation {
            name,
            path,
            kind,
            branch,
            existing: false,
            created_branch,
            warnings,
            hooks: Vec::new(),
        }),
        Err(e) => {
            if created_branch && let Ok(mut b) = repo.find_branch(&branch, BranchType::Local) {
                let _ = b.delete();
            }
            Err(e)
        }
    }
}

/// Create a new local `branch` from `base`: a local branch, the default
/// remote's `<base>`, or any revision (a tag or SHA). Returns the remote the
/// base tracks, for LFS content.
fn create_branch_from_base(repo: &Repository, branch: &str, base: &str) -> Result<Option<String>> {
    if repo.find_branch(branch, BranchType::Local).is_ok() {
        bail!("branch {branch} already exists");
    }
    let local = repo
        .find_branch(base, BranchType::Local)
        .ok()
        .and_then(|b| b.get().peel_to_commit().ok());
    let remote = default_remote(repo).and_then(|r| {
        repo.find_reference(&format!("refs/remotes/{r}/{base}"))
            .ok()?
            .peel_to_commit()
            .ok()
    });
    let commit = match local.or(remote) {
        Some(c) => c,
        None => repo
            .revparse_single(base)
            .and_then(|o| o.peel_to_commit())
            .with_context(|| format!("resolve base {base}"))?,
    };
    repo.branch(branch, &commit, false)
        .with_context(|| format!("create branch {branch} from {base}"))?;
    Ok(base_remote(repo, base))
}

/// Make sure local branch `branch` exists. Returns the remote to pull LFS
/// content from and whether swamp created the branch.
fn ensure_local_branch(
    repo: &Repository,
    branch: &str,
    default: &str,
) -> Result<(Option<String>, bool)> {
    if repo.find_branch(branch, BranchType::Local).is_ok() {
        return Ok((branch_remote(repo, branch), false));
    }
    if let Some((remote, oid)) = find_remote_tracking_branch(repo, branch) {
        let commit = repo.find_commit(oid)?;
        let mut local = repo
            .branch(branch, &commit, false)
            .with_context(|| format!("create branch {branch} from {remote}/{branch}"))?;
        let _ = local.set_upstream(Some(&format!("{remote}/{branch}")));
        return Ok((Some(remote), true));
    }
    let (base, remote) = new_branch_base(repo, default)?;
    repo.branch(branch, &base, false)
        .with_context(|| format!("create branch {branch}"))?;
    Ok((remote, true))
}

/// The commit a brand-new branch starts from: `<remote>/<default>`, else the
/// local default branch, else the repository `HEAD`.
fn new_branch_base<'r>(
    repo: &'r Repository,
    default: &str,
) -> Result<(git2::Commit<'r>, Option<String>)> {
    if let Some(remote) = default_remote(repo)
        && !default.is_empty()
        && let Ok(r) = repo.find_reference(&format!("refs/remotes/{remote}/{default}"))
    {
        return Ok((r.peel_to_commit()?, Some(remote)));
    }
    if !default.is_empty()
        && let Ok(b) = repo.find_branch(default, BranchType::Local)
    {
        return Ok((b.get().peel_to_commit()?, None));
    }
    let head = repo
        .head()
        .context("no default branch and no HEAD to start a new branch from")?;
    Ok((head.peel_to_commit()?, None))
}

/// Put `branch` into a free slot, a new slot, or an overflow worktree.
fn place(
    repo: &Repository,
    common: &Path,
    worktrees: &[Worktree],
    branch: &str,
    remote: Option<&str>,
    opts: &AllocOptions,
) -> Result<(String, PathBuf, WorktreeKind)> {
    let mut slots: Vec<(u32, &Worktree)> = worktrees
        .iter()
        .filter_map(|w| slot_number(&w.name()).map(|n| (n, w)))
        .filter(|(n, _)| *n <= opts.pool_size)
        .collect();
    slots.sort_by_key(|(n, _)| *n);

    for (_, wt) in &slots {
        let name = wt.name();
        if classify(common, &name, &wt.path).0 != SlotState::Free {
            continue;
        }
        match checkout_branch_in(&wt.path, branch) {
            Ok(()) => {
                inflate_lfs(&wt.path, remote);
                return Ok((name, wt.path.clone(), WorktreeKind::Slot));
            }
            Err(e) => {
                tracing::warn!(slot = %name, "checkout of {branch} failed, trying next: {e:#}")
            }
        }
    }

    let reference = repo
        .find_branch(branch, BranchType::Local)?
        .into_reference();
    let root = workon_root(repo)?;
    let taken: Vec<String> = worktrees.iter().map(Worktree::name).collect();
    if let Some(n) = (1..=opts.pool_size)
        .find(|n| !taken.contains(&slot_name(*n)) && !root.join(slot_name(*n)).exists())
    {
        let wt = add_worktree_named(repo, &slot_name(n), branch, &reference, remote)?;
        return Ok((slot_name(n), wt.path, WorktreeKind::Slot));
    }

    let name = overflow_name_for_branch(branch);
    let wt = add_worktree_named(repo, &name, branch, &reference, remote)?;
    Ok((name, wt.path, WorktreeKind::Overflow))
}

/// Check `branch` out over the existing slot at `path`. The checkout is safe:
/// it refuses to overwrite files it cannot restore, and it refuses to
/// overwrite an ignored file (such as a local `.env`) that `branch` tracks.
/// Other ignored files (build output) stay in place.
fn checkout_branch_in(path: &Path, branch: &str) -> Result<()> {
    let repo = Repository::open(path).with_context(|| format!("open {}", path.display()))?;
    let refname = format!("refs/heads/{branch}");
    let target = repo
        .revparse_single(&refname)
        .with_context(|| format!("resolve {refname}"))?;
    let mut co = CheckoutBuilder::new();
    co.safe().overwrite_ignored(false);
    repo.checkout_tree(&target, Some(&mut co))
        .with_context(|| format!("check out {branch} in {}", path.display()))?;
    repo.set_head(&refname)
        .with_context(|| format!("set HEAD to {refname}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Release

/// PR facts for the branch a release targets, gathered by the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrFacts {
    /// The branch the facts describe. Release ignores facts for another branch.
    pub branch: String,
    /// GitHub PR state: `OPEN`, `MERGED`, or `CLOSED`.
    pub state: String,
    #[serde(default, with = "oid_opt")]
    pub head_oid: Option<Oid>,
}

mod oid_opt {
    use git2::Oid;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(oid: &Option<Oid>, s: S) -> Result<S::Ok, S::Error> {
        match oid {
            Some(o) => s.serialize_some(&o.to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Oid>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        Ok(s.and_then(|s| s.parse().ok()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseOptions {
    pub force: bool,
    /// Fetch the branch from the default remote first (best effort).
    pub fetch: bool,
    pub pr: Option<PrFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseOutcome {
    /// Swamp detached the slot. `branch_deleted` is true when the branch's PR
    /// was finished and swamp deleted the local branch.
    Detached { branch_deleted: bool },
    /// The slot was already detached; swamp only cleared its pin.
    AlreadyFree,
    /// The target is an overflow worktree, which persists until the user
    /// removes it.
    KeptOverflow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub name: String,
    pub path: PathBuf,
    pub kind: WorktreeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub outcome: ReleaseOutcome,
    /// `pre-release` hook results, for the caller to show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<crate::slot_hooks::HookRun>,
}

/// Why a non-forced release was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseRefusedReason {
    Dirty,
    OperationInProgress,
    UnpushedCommits(u32),
    NotOnRemote,
    DefaultBranchWorktree,
    StatusUnreadable,
    /// Another slot's agent is working; the caller does not run in that slot.
    AgentWorking,
    /// A `pre-release` hook failed: the command with its error, and its output.
    HookFailed {
        hook: String,
        output: String,
    },
}

#[derive(Debug)]
pub struct ReleaseRefused {
    pub name: String,
    pub branch: Option<String>,
    pub reason: ReleaseRefusedReason,
}

impl std::fmt::Display for ReleaseRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let on = self
            .branch
            .as_deref()
            .map(|b| format!(" on {b}"))
            .unwrap_or_default();
        let why = match &self.reason {
            ReleaseRefusedReason::Dirty => "has modified or untracked files".to_string(),
            ReleaseRefusedReason::OperationInProgress => {
                "has a merge, rebase, or similar operation in progress".to_string()
            }
            ReleaseRefusedReason::UnpushedCommits(n) => {
                format!("has {n} unpushed commit(s) and no merged or closed PR")
            }
            ReleaseRefusedReason::NotOnRemote => {
                "has commits that are not on the remote and no merged or closed PR".to_string()
            }
            ReleaseRefusedReason::DefaultBranchWorktree => {
                "holds the default branch and is not a slot".to_string()
            }
            ReleaseRefusedReason::StatusUnreadable => "has unreadable git status".to_string(),
            ReleaseRefusedReason::AgentWorking => {
                "has a working agent and you do not run inside it".to_string()
            }
            ReleaseRefusedReason::HookFailed { hook, .. } => {
                format!("was not released because pre-release hook {hook} failed")
            }
        };
        write!(f, "{}{on} {why}; leaving it in place", self.name)?;
        if let ReleaseRefusedReason::HookFailed { output, .. } = &self.reason
            && !output.trim().is_empty()
        {
            write!(f, "\n{}", output.trim_end())?;
        }
        Ok(())
    }
}

impl std::error::Error for ReleaseRefused {}

/// Find the worktree a release, pin, or title command means. `target` is a
/// worktree name, a path, or a branch name; `None` means the worktree that
/// contains `cwd`.
pub fn resolve_target(common: &Path, target: Option<&str>, cwd: &Path) -> Result<Worktree> {
    let worktrees = list_worktrees(common)?;
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let containing = |dir: &Path| {
        let dir = canon(dir);
        worktrees
            .iter()
            .filter(|w| dir.starts_with(canon(&w.path)))
            .max_by_key(|w| canon(&w.path).components().count())
            .cloned()
    };
    let Some(target) = target else {
        return containing(cwd)
            .with_context(|| format!("{} is not inside a worktree", cwd.display()));
    };
    if let Some(wt) = worktrees.iter().find(|w| w.name() == target) {
        return Ok(wt.clone());
    }
    let as_path = Path::new(target);
    if as_path.exists()
        && let Some(wt) = containing(as_path)
    {
        return Ok(wt);
    }
    if let Ok(branch) = sanitize_branch_name(target)
        && let Some(wt) = worktrees
            .iter()
            .find(|w| checked_out_branch(&w.path).as_deref() == Some(branch.as_str()))
    {
        return Ok(wt.clone());
    }
    bail!("no worktree, path, or checked-out branch matches '{target}'")
}

/// Return a slot to the pool. See the module docs and [`ReleaseRefusedReason`].
/// Production callers go through [`crate::slot_service::release`].
#[cfg(test)]
pub fn release(
    common: &Path,
    target: Option<&str>,
    cwd: &Path,
    opts: &ReleaseOptions,
) -> Result<Release> {
    let guard = lock(common)?;
    let wt = resolve_target(common, target, cwd)?;
    release_locked(&guard, common, &wt, opts, |_| Ok(()))
}

/// [`release`] for a caller that holds the slot lock and has resolved the
/// target worktree. `before_detach` runs after every check passes and right
/// before swamp detaches the slot, with the branch name; an error from it
/// stops the release with nothing changed.
pub fn release_locked(
    _lock: &SlotLock,
    common: &Path,
    wt: &Worktree,
    opts: &ReleaseOptions,
    before_detach: impl FnOnce(&str) -> Result<()>,
) -> Result<Release> {
    let name = wt.name();
    let common_repo =
        Repository::open(common).with_context(|| format!("open repo at {}", common.display()))?;
    let default = default_branch_name(&common_repo).unwrap_or_default();
    let branch = checked_out_branch(&wt.path);
    let kind = worktree_kind(&name, branch.as_deref().unwrap_or(""), &default);
    let refused = |reason| ReleaseRefused {
        name: name.clone(),
        branch: branch.clone(),
        reason,
    };
    let done = |outcome| Release {
        name: name.clone(),
        path: wt.path.clone(),
        kind,
        branch: branch.clone(),
        outcome,
        hooks: Vec::new(),
    };

    match kind {
        WorktreeKind::Default => {
            return Err(refused(ReleaseRefusedReason::DefaultBranchWorktree).into());
        }
        WorktreeKind::Overflow => return Ok(done(ReleaseOutcome::KeptOverflow)),
        WorktreeKind::Slot => {}
    }
    let Some(branch_name) = branch.clone() else {
        clear_pin(common, &name);
        return Ok(done(ReleaseOutcome::AlreadyFree));
    };

    let remote = default_remote(&common_repo);
    if opts.fetch
        && let Some(remote) = &remote
    {
        let spec = format!("refs/heads/{branch_name}:refs/remotes/{remote}/{branch_name}");
        // Missing on the remote is normal for a never-pushed branch.
        let _ = git_subprocess(common, &["fetch", "--quiet", remote, &spec]);
    }

    let slot_repo =
        Repository::open(&wt.path).map_err(|_| refused(ReleaseRefusedReason::StatusUnreadable))?;
    let tip = slot_repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .ok_or_else(|| refused(ReleaseRefusedReason::StatusUnreadable))?;
    let pr = opts.pr.as_ref().filter(|p| p.branch == branch_name);
    let pr_finished = pr.is_some_and(|p| {
        matches!(p.state.as_str(), "MERGED" | "CLOSED") && p.head_oid.is_none_or(|h| h == tip)
    });

    if !opts.force {
        let info: GitInfo =
            git_info(&wt.path).map_err(|_| refused(ReleaseRefusedReason::StatusUnreadable))?;
        if info.is_dirty() {
            return Err(refused(ReleaseRefusedReason::Dirty).into());
        }
        if info.rebase || slot_repo.state() != RepositoryState::Clean {
            return Err(refused(ReleaseRefusedReason::OperationInProgress).into());
        }
        if !pr_finished {
            let ahead = info.ahead;
            let ctx = VerdictContext {
                git_info: info,
                default_branch: default.clone(),
                default_branch_tip: super::branches::default_branch_tip_in_repo(&common_repo),
                pr_state: pr.map(|p| p.state.clone()),
                pr_head_oid: pr.and_then(|p| p.head_oid),
            };
            match branch_safety(&common_repo, &ctx) {
                RemovalVerdict::Removable => {}
                RemovalVerdict::Blocked(RemoveRefusedReason::UnpushedCommits) => {
                    return Err(refused(ReleaseRefusedReason::UnpushedCommits(ahead)).into());
                }
                RemovalVerdict::Blocked(_) => {
                    return Err(refused(ReleaseRefusedReason::NotOnRemote).into());
                }
            }
        }
    }

    before_detach(&branch_name)?;
    slot_repo
        .set_head_detached(tip)
        .with_context(|| format!("detach {name}"))?;
    clear_pin(common, &name);

    let mut branch_deleted = false;
    if pr_finished && let Ok(mut b) = common_repo.find_branch(&branch_name, BranchType::Local) {
        // A squash merge leaves the branch unmerged in git's view, so this is
        // a force delete. The PR check above is what makes it safe.
        branch_deleted = b.delete().is_ok();
        if branch_deleted
            && let Some(remote) = &remote
            && let Ok(mut r) =
                common_repo.find_reference(&format!("refs/remotes/{remote}/{branch_name}"))
        {
            let _ = r.delete();
        }
    }
    Ok(done(ReleaseOutcome::Detached { branch_deleted }))
}

// ---------------------------------------------------------------------------
// Pins

/// Pin free slot `slot` so allocation skips it. A busy slot is refused.
pub fn pin(common: &Path, slot: &str, reason: &str) -> Result<()> {
    let _lock = lock(common)?;
    let wt = find_slot(common, slot)?;
    if classify(common, slot, &wt.path).0 == SlotState::Busy {
        bail!("{slot} is busy; only a free slot can be pinned");
    }
    let path = pin_path(common, slot);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let reason = reason.trim();
    let reason = if reason.is_empty() { "pinned" } else { reason };
    std::fs::write(&path, format!("{reason}\n"))
        .with_context(|| format!("write {}", path.display()))
}

/// Remove the pin on `slot`. Returns false when no pin existed.
pub fn unpin(common: &Path, slot: &str) -> Result<bool> {
    let _lock = lock(common)?;
    find_slot(common, slot)?;
    Ok(clear_pin(common, slot))
}

fn find_slot(common: &Path, slot: &str) -> Result<Worktree> {
    if slot_number(slot).is_none() {
        bail!("'{slot}' is not a slot name (expected work<N>)");
    }
    list_worktrees(common)?
        .into_iter()
        .find(|w| w.name() == slot)
        .with_context(|| format!("slot {slot} does not exist"))
}

/// Run `git --git-dir <common> <args>` with stdout sent to stderr. Stdout is
/// reserved for the path that alloc prints.
fn git_subprocess(common: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(common)
        .args(args)
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::clone_repo;
    use crate::worktree::test_support::{git_available, run, setup};

    fn opts() -> AllocOptions {
        AllocOptions::pool(5)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d =
            std::env::temp_dir().join(format!("swamp-slots-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A swamp bare-layout clone of a local origin that has `main` (with a
    /// `.gitignore` for `target/`) and a remote-only branch `feature`.
    /// Returns `(scratch, origin, common)`.
    fn cloned() -> (PathBuf, PathBuf, PathBuf) {
        let scratch = temp_dir("clone");
        let origin = scratch.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        run(&origin, &["init", "-q", "-b", "main"]);
        identity(&origin);
        std::fs::write(origin.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(origin.join("a.txt"), "a\n").unwrap();
        run(&origin, &["add", "."]);
        run(&origin, &["commit", "-q", "-m", "init"]);
        run(&origin, &["branch", "feature"]);
        let root = scratch.join("app");
        clone_repo(&format!("file://{}", origin.display()), Some(&root)).unwrap();
        identity(&root.join(".bare"));
        (scratch, origin, root.join(".bare"))
    }

    fn identity(dir: &Path) {
        run(dir, &["config", "user.email", "t@example.com"]);
        run(dir, &["config", "user.name", "t"]);
    }

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-q", "-m", file]);
    }

    fn head_oid(dir: &Path) -> Oid {
        Repository::open(dir)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
    }

    fn local_branch_exists(common: &Path, b: &str) -> bool {
        Repository::open(common)
            .unwrap()
            .find_branch(b, BranchType::Local)
            .is_ok()
    }

    fn release_slot(common: &Path, slot: &str, opts: &ReleaseOptions) -> Result<Release> {
        release(common, Some(slot), Path::new("/"), opts)
    }

    fn refusal(err: anyhow::Error) -> ReleaseRefusedReason {
        err.downcast::<ReleaseRefused>().unwrap().reason
    }

    #[test]
    fn slot_names_parse() {
        assert_eq!(slot_number("work1"), Some(1));
        assert_eq!(slot_number("work12"), Some(12));
        assert_eq!(slot_number("work0"), None);
        assert_eq!(slot_number("work01"), None);
        assert_eq!(slot_number("work3-1"), None);
        assert_eq!(slot_number("workx"), None);
        assert_eq!(slot_number("work"), None);
        assert_eq!(slot_number("main"), None);
    }

    #[test]
    fn kinds_follow_names_and_default_branch() {
        assert_eq!(worktree_kind("work2", "main", "main"), WorktreeKind::Slot);
        assert_eq!(worktree_kind("main", "main", "main"), WorktreeKind::Default);
        assert_eq!(worktree_kind("fix", "fix", "main"), WorktreeKind::Overflow);
        assert_eq!(worktree_kind("main", "main", ""), WorktreeKind::Overflow);
    }

    #[test]
    fn overflow_names_avoid_slot_form() {
        assert_eq!(overflow_name_for_branch("feature"), "feature");
        assert_eq!(overflow_name_for_branch("alice/fix"), "alice-fix");
        assert_eq!(overflow_name_for_branch("work3"), "work3-1");
        assert_eq!(overflow_name_for_branch("work9"), "work9-1");
    }

    #[test]
    fn branch_names_are_sanitized() {
        assert_eq!(sanitize_branch_name("parser fix").unwrap(), "parser-fix");
        assert_eq!(sanitize_branch_name("a//b").unwrap(), "a/b");
        assert_eq!(sanitize_branch_name("--x").unwrap(), "x");
        assert_eq!(sanitize_branch_name("fix~1^").unwrap(), "fix-1-");
        assert!(sanitize_branch_name("").is_err());
        assert!(sanitize_branch_name("a..b").is_err());
    }

    #[test]
    fn first_alloc_creates_work1_with_new_branch() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();

        let a = alloc(&bare, "parser-fix", &opts()).unwrap();

        assert_eq!(a.name, "work1");
        assert_eq!(a.kind, WorktreeKind::Slot);
        assert!(a.created_branch && !a.existing);
        assert_eq!(checked_out_branch(&a.path).as_deref(), Some("parser-fix"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn busy_slots_lead_to_next_slot_then_overflow() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(2);

        assert_eq!(alloc(&bare, "a", &opts).unwrap().name, "work1");
        assert_eq!(alloc(&bare, "b", &opts).unwrap().name, "work2");
        let c = alloc(&bare, "c", &opts).unwrap();

        assert_eq!(c.name, "c");
        assert_eq!(c.kind, WorktreeKind::Overflow);
        assert!(!root.join("work3").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pool_size_zero_always_overflows() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(0);

        let a = alloc(&bare, "a", &opts).unwrap();

        assert_eq!(a.kind, WorktreeKind::Overflow);
        assert_eq!(a.name, "a");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn alloc_returns_existing_worktree_for_checked_out_branch() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let first = alloc(&bare, "fix-ci", &opts()).unwrap();

        let again = alloc(&bare, "fix-ci", &opts()).unwrap();

        assert!(again.existing);
        assert_eq!(again.path, first.path);
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn alloc_reuses_free_slot_and_keeps_ignored_build_output() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "one", &opts()).unwrap();
        std::fs::create_dir_all(a.path.join("target")).unwrap();
        std::fs::write(a.path.join("target/cache.bin"), "warm").unwrap();
        // Make `one` safe to release: it equals origin/main.
        release_slot(&common, "work1", &ReleaseOptions::default()).unwrap();

        let b = alloc(&common, "two", &opts()).unwrap();

        assert_eq!(b.name, "work1");
        assert_eq!(checked_out_branch(&b.path).as_deref(), Some("two"));
        assert_eq!(
            std::fs::read_to_string(b.path.join("target/cache.bin")).unwrap(),
            "warm"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn alloc_skips_slot_whose_ignored_file_the_branch_tracks() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "tracked", &opts()).unwrap();
        std::fs::create_dir_all(a.path.join("target")).unwrap();
        std::fs::write(a.path.join("target/x"), "branch").unwrap();
        run(&a.path, &["add", "-f", "target/x"]);
        run(&a.path, &["commit", "-q", "-m", "track target/x"]);
        run(&a.path, &["checkout", "-q", "--detach", "origin/main"]);
        // work1 is now free, with an ignored local file at the same path.
        std::fs::create_dir_all(a.path.join("target")).unwrap();
        std::fs::write(a.path.join("target/x"), "local").unwrap();

        let b = alloc(&common, "tracked", &opts()).unwrap();

        assert_eq!(b.name, "work2");
        assert_eq!(
            std::fs::read_to_string(a.path.join("target/x")).unwrap(),
            "local"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn alloc_tracks_remote_only_branch() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();

        let a = alloc(&common, "feature", &opts()).unwrap();

        assert!(a.created_branch);
        let repo = Repository::open(&common).unwrap();
        let b = repo.find_branch("feature", BranchType::Local).unwrap();
        assert_eq!(
            b.upstream().unwrap().name().unwrap(),
            Some("origin/feature")
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn new_branch_starts_from_remote_default_without_upstream() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        // Move local main ahead so the base choice is observable.
        let main = common.parent().unwrap().join("main");
        commit(&main, "local-only.txt");

        let a = alloc(&common, "fresh", &opts()).unwrap();

        let repo = Repository::open(&common).unwrap();
        let origin_main = repo
            .find_reference("refs/remotes/origin/main")
            .unwrap()
            .target()
            .unwrap();
        assert_eq!(head_oid(&a.path), origin_main);
        assert!(
            repo.find_branch("fresh", BranchType::Local)
                .unwrap()
                .upstream()
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn concurrent_allocs_get_different_worktrees() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let handles: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|b| {
                let bare = bare.clone();
                std::thread::spawn(move || alloc(&bare, b, &opts()).unwrap().path)
            })
            .collect();

        let mut paths: Vec<PathBuf> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        paths.sort();
        paths.dedup();

        assert_eq!(paths.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn classify_reports_each_state() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let a = alloc(&bare, "a", &opts()).unwrap();
        assert_eq!(classify(&bare, "work1", &a.path).0, SlotState::Busy);

        let repo = Repository::open(&a.path).unwrap();
        repo.set_head_detached(head_oid(&a.path)).unwrap();
        assert_eq!(classify(&bare, "work1", &a.path).0, SlotState::Free);

        std::fs::write(a.path.join("untracked.txt"), "x").unwrap();
        assert_eq!(classify(&bare, "work1", &a.path).0, SlotState::Busy);
        std::fs::remove_file(a.path.join("untracked.txt")).unwrap();

        pin(&bare, "work1", "review #172").unwrap();
        assert_eq!(
            classify(&bare, "work1", &a.path),
            (SlotState::Pinned, Some("review #172".into()))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn alloc_skips_pinned_slot() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let a = alloc(&bare, "a", &opts()).unwrap();
        Repository::open(&a.path)
            .unwrap()
            .set_head_detached(head_oid(&a.path))
            .unwrap();
        pin(&bare, "work1", "review").unwrap();

        assert_eq!(alloc(&bare, "b", &opts()).unwrap().name, "work2");

        assert!(unpin(&bare, "work1").unwrap());
        assert_eq!(alloc(&bare, "c", &opts()).unwrap().name, "work1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pin_refuses_busy_slot() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        alloc(&bare, "a", &opts()).unwrap();

        let err = pin(&bare, "work1", "x").unwrap_err();

        assert!(err.to_string().contains("busy"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn release_pushed_branch_keeps_branch() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "fix-ci", &opts()).unwrap();
        commit(&a.path, "fix.txt");
        run(&a.path, &["push", "-q", "origin", "fix-ci"]);

        let r = release_slot(&common, "work1", &ReleaseOptions::default()).unwrap();

        assert_eq!(
            r.outcome,
            ReleaseOutcome::Detached {
                branch_deleted: false
            }
        );
        assert!(Repository::open(&a.path).unwrap().head_detached().unwrap());
        assert!(local_branch_exists(&common, "fix-ci"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn release_refuses_dirty_slot() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "wip", &opts()).unwrap();
        std::fs::write(a.path.join("scratch.txt"), "x").unwrap();

        let err = release_slot(&common, "work1", &ReleaseOptions::default()).unwrap_err();

        assert_eq!(refusal(err), ReleaseRefusedReason::Dirty);
        assert_eq!(checked_out_branch(&a.path).as_deref(), Some("wip"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn release_refuses_unpushed_commits_without_finished_pr() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "feature", &opts()).unwrap();
        commit(&a.path, "one.txt");
        commit(&a.path, "two.txt");

        let err = release_slot(&common, "work1", &ReleaseOptions::default()).unwrap_err();

        assert_eq!(refusal(err), ReleaseRefusedReason::UnpushedCommits(2));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn release_refuses_never_pushed_branch() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "local", &opts()).unwrap();
        commit(&a.path, "one.txt");

        let err = release_slot(&common, "work1", &ReleaseOptions::default()).unwrap_err();

        assert_eq!(refusal(err), ReleaseRefusedReason::NotOnRemote);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn release_with_merged_pr_deletes_squash_merged_branch() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "squashed", &opts()).unwrap();
        commit(&a.path, "one.txt");
        let tip = head_oid(&a.path);
        let opts = ReleaseOptions {
            pr: Some(PrFacts {
                branch: "squashed".into(),
                state: "MERGED".into(),
                head_oid: Some(tip),
            }),
            ..Default::default()
        };

        let r = release_slot(&common, "work1", &opts).unwrap();

        assert_eq!(
            r.outcome,
            ReleaseOutcome::Detached {
                branch_deleted: true
            }
        );
        assert!(!local_branch_exists(&common, "squashed"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn finished_pr_with_newer_local_commits_is_not_enough() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "more", &opts()).unwrap();
        commit(&a.path, "one.txt");
        let pr_head = head_oid(&a.path);
        commit(&a.path, "two.txt");
        let opts = ReleaseOptions {
            pr: Some(PrFacts {
                branch: "more".into(),
                state: "CLOSED".into(),
                head_oid: Some(pr_head),
            }),
            ..Default::default()
        };

        assert!(release_slot(&common, "work1", &opts).is_err());
        assert!(local_branch_exists(&common, "more"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn release_of_free_slot_clears_pin() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let a = alloc(&bare, "a", &opts()).unwrap();
        Repository::open(&a.path)
            .unwrap()
            .set_head_detached(head_oid(&a.path))
            .unwrap();
        pin(&bare, "work1", "review").unwrap();

        let r = release_slot(&bare, "work1", &ReleaseOptions::default()).unwrap();

        assert_eq!(r.outcome, ReleaseOutcome::AlreadyFree);
        assert_eq!(read_pin(&bare, "work1"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn release_keeps_overflow_worktree() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let opts = AllocOptions::pool(0);
        let a = alloc(&bare, "spike", &opts).unwrap();

        let r = release(
            &bare,
            Some("spike"),
            Path::new("/"),
            &ReleaseOptions::default(),
        )
        .unwrap();

        assert_eq!(r.outcome, ReleaseOutcome::KeptOverflow);
        assert_eq!(checked_out_branch(&a.path).as_deref(), Some("spike"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn release_targets_cwd_and_branch_name() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();
        let a = alloc(&common, "by-branch", &opts()).unwrap();

        let by_cwd = resolve_target(&common, None, &a.path.join("sub")).ok();
        let by_branch = resolve_target(&common, Some("by-branch"), Path::new("/")).unwrap();

        // `sub` does not exist, so the cwd lookup falls back to its own path.
        assert!(by_cwd.is_none() || by_cwd.unwrap().path == a.path);
        assert_eq!(by_branch.path, a.path);
        let by_path = resolve_target(&common, None, &a.path).unwrap();
        assert_eq!(by_path.path, a.path);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn release_refuses_default_branch_worktree() {
        if !git_available() {
            return;
        }
        let (scratch, _origin, common) = cloned();

        let err = release(
            &common,
            Some("main"),
            Path::new("/"),
            &ReleaseOptions::default(),
        )
        .unwrap_err();

        assert_eq!(refusal(err), ReleaseRefusedReason::DefaultBranchWorktree);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn list_marks_slots_outside_pool() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        for b in ["a", "b", "c"] {
            alloc(&bare, b, &opts()).unwrap();
        }
        let overflow = AllocOptions::pool(0);
        alloc(&bare, "spike", &overflow).unwrap();

        let entries = list_pool(&bare, 2).unwrap();

        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["work1", "work2", "work3", "spike"]);
        assert!(entries[0].in_pool && entries[1].in_pool && !entries[2].in_pool);
        assert_eq!(entries[0].state, Some(SlotState::Busy));
        assert_eq!(entries[0].branch.as_deref(), Some("a"));
        assert_eq!(entries[3].kind, WorktreeKind::Overflow);
        assert_eq!(entries[3].state, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn alloc_skips_slots_above_pool_size() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        for b in ["a", "b", "c"] {
            alloc(&bare, b, &opts()).unwrap();
        }
        // Free work3, then shrink the pool to 2: work3 must not be reused.
        let w3 = root.join("work3");
        Repository::open(&w3)
            .unwrap()
            .set_head_detached(head_oid(&w3))
            .unwrap();
        let small = AllocOptions::pool(2);

        let d = alloc(&bare, "d", &small).unwrap();

        assert_eq!(d.kind, WorktreeKind::Overflow);
        let _ = std::fs::remove_dir_all(&root);
    }
}
